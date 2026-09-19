//! End-to-end LSP request/response tests.
//!
//! These tests spawn the real `symfony-lsp` binary, initialize it against a
//! temporary Symfony fixture project, and drive it over stdio JSON-RPC. They
//! exercise the full path a Zed client uses: framing, `initialize`, indexing,
//! document sync, and the hover/definition/references/completion/diagnostics/
//! code-action/rename handlers. This complements the parser-level unit tests,
//! which call internal helpers directly.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use tower_lsp::lsp_types::Url;

/// Fixture files written for every test. Contents are intentionally small but
/// cover the syntax forms the release scope advertises.
const FILES: &[(&str, &str)] = &[
    (
        "composer.json",
        r#"{"require":{"php":">=8.1","symfony/framework-bundle":"^7.0"}}"#,
    ),
    (
        "config/routes.yaml",
        r#"hello_world:
    path: /hello/{name}
    controller: App\Controller\HelloController::index
"#,
    ),
    (
        "config/services.yaml",
        r#"services:
    mailer.transport: ~
    App\Service\Mailer: ~
    App\Service\Newsletter:
        arguments: ['@mailer.transport']
    App\Service\Reporter:
        arguments: ['@missing.service']
    App\EventSubscriber\RequestEventSubscriber:
        class: App\EventSubscriber\RequestEventSubscriber
"#,
    ),
    (
        "config/services.extra.yaml",
        r#"services:
    App\Service\Greeter:
        arguments: ['@mail']
"#,
    ),
    (
        "src/Controller/HelloController.php",
        r#"<?php
namespace App\Controller;

class HelloController
{
    public function index(string $name): string
    {
        $env = env('APP_ENV');
        $missing = env('MISSING_KEY');
        $this->redirectToRoute('missing_route');
        return $this->render('page.html.twig', ['name' => $name]);
    }

    public function preview(): string
    {
        return 'ok';
    }
}
"#,
    ),
    (
        "src/Controller/ReportController.php",
        r#"<?php
namespace App\Controller;

class ReportController
{
    public function send(): void
    {
        $this->mailer->send();
    }
}
"#,
    ),
    (
        "src/Service/Mailer.php",
        r#"<?php
namespace App\Service;

class Mailer
{
    public function send(): void {}
}
"#,
    ),
    (
        "src/Config.php",
        r#"<?php
namespace App;

class Config
{
    public function boot(): void
    {
        $value = env('APP');
    }
}
"#,
    ),
    (
        "templates/base.html.twig",
        r#"<!doctype html>
<html><body>{% block body %}{% endblock %}</body></html>
"#,
    ),
    (
        "templates/page.html.twig",
        r#"{% extends 'base.html.twig' %}
{% block body %}
{{ path('hello_world') }}
{{ path('hello_world', ) }}
{{ 'greeting'|trans }}
{{ 'absent'|trans }}
{{ include('missing.html.twig') }}
{% endblock %}
"#,
    ),
    (
        "templates/search.html.twig",
        r#"{{ include('pa') }}
"#,
    ),
    (
        "templates/trans.html.twig",
        r#"{{ 'gre'|trans }}
"#,
    ),
    (
        "translations/messages.en.yaml",
        r#"greeting: Hello
"#,
    ),
    (".env", "APP_ENV=dev\nDATABASE_URL=sqlite:///var/data.db\n"),
];

/// A temporary Symfony project on disk.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("symfony-lsp-e2e-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (relative, contents) in FILES {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
        }
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn text(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).unwrap()
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn remove(&self, relative: &str) {
        let _ = fs::remove_file(self.path(relative));
    }

    fn start(&self) -> Lsp {
        let mut lsp = Lsp::spawn();
        lsp.initialize(&self.root);
        lsp
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Minimal stdio JSON-RPC client for the language server.
struct Lsp {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    notifications: Vec<Value>,
    next_id: i64,
}

impl Lsp {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_symfony-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("RUST_LOG", "error")
            .spawn()
            .expect("failed to spawn symfony-lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Self {
            child,
            stdin,
            messages: spawn_reader(stdout),
            notifications: Vec::new(),
            next_id: 1,
        }
    }

    fn initialize(&mut self, root: &Path) {
        self.initialize_with_capabilities(root, json!({}));
    }

    fn initialize_with_capabilities(&mut self, root: &Path, capabilities: Value) {
        let root_uri = Url::from_file_path(root).unwrap();
        let response = self.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": capabilities,
            }),
        );
        assert!(
            response.get("error").is_none(),
            "initialize failed: {response}"
        );
        self.send(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_vec(&message).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.stdin.write_all(&body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self, what: &str) -> Value {
        match self.messages.recv_timeout(Duration::from_secs(30)) {
            Ok(message) => message,
            Err(error) => panic!("timed out waiting for {what}: {error:?}"),
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let message = self.recv(method);
            if message.get("method").is_none()
                && message.get("id").and_then(Value::as_i64) == Some(id)
            {
                return message;
            }
            self.absorb(message);
        }
    }

    /// Handle a server-to-client message. Server-initiated requests (for example
    /// `client/registerCapability`) are acknowledged so the server does not block
    /// waiting for a reply, and every method-bearing message is retained for
    /// later assertions.
    fn absorb(&mut self, message: Value) {
        if message.get("method").is_none() {
            return;
        }
        if let Some(id) = message.get("id") {
            self.send(json!({"jsonrpc": "2.0", "id": id, "result": null}));
        }
        self.notifications.push(message);
    }

    fn result(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        assert!(
            response.get("error").is_none(),
            "{method} returned an error: {response}"
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn wait_for(&mut self, what: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        if let Some(position) = self.notifications.iter().position(&predicate) {
            return self.notifications.remove(position);
        }
        loop {
            let message = self.recv(what);
            if predicate(&message) {
                if message.get("method").is_some() {
                    if let Some(id) = message.get("id") {
                        self.send(json!({"jsonrpc": "2.0", "id": id, "result": null}));
                    }
                }
                return message;
            }
            self.absorb(message);
        }
    }

    fn open(&mut self, path: &Path, language_id: &str) {
        let text = fs::read_to_string(path).unwrap();
        let uri = Url::from_file_path(path).unwrap();
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }
        }));
    }

    fn diagnostics_for(&mut self, path: &Path) -> Vec<Value> {
        let uri = Url::from_file_path(path).unwrap().to_string();
        let message = self.wait_for("publishDiagnostics", |message| {
            message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
                && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
        });
        message
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn definition(&mut self, path: &Path, position: (u32, u32)) -> Value {
        self.result(
            "textDocument/definition",
            json!({
                "textDocument": {"uri": Url::from_file_path(path).unwrap()},
                "position": {"line": position.0, "character": position.1},
            }),
        )
    }

    fn hover(&mut self, path: &Path, position: (u32, u32)) -> Value {
        self.result(
            "textDocument/hover",
            json!({
                "textDocument": {"uri": Url::from_file_path(path).unwrap()},
                "position": {"line": position.0, "character": position.1},
            }),
        )
    }

    fn references(&mut self, path: &Path, position: (u32, u32)) -> Value {
        self.result(
            "textDocument/references",
            json!({
                "textDocument": {"uri": Url::from_file_path(path).unwrap()},
                "position": {"line": position.0, "character": position.1},
                "context": {"includeDeclaration": true},
            }),
        )
    }

    fn completion(&mut self, path: &Path, position: (u32, u32)) -> Value {
        self.result(
            "textDocument/completion",
            json!({
                "textDocument": {"uri": Url::from_file_path(path).unwrap()},
                "position": {"line": position.0, "character": position.1},
            }),
        )
    }

    fn code_action(&mut self, path: &Path, line: u32, diagnostics: &[Value]) -> Value {
        self.result(
            "textDocument/codeAction",
            json!({
                "textDocument": {"uri": Url::from_file_path(path).unwrap()},
                "range": {
                    "start": {"line": line, "character": 0},
                    "end": {"line": line, "character": 0},
                },
                "context": {"diagnostics": diagnostics},
            }),
        )
    }

    fn prepare_rename(&mut self, path: &Path, position: (u32, u32)) -> Value {
        self.result(
            "textDocument/prepareRename",
            json!({
                "textDocument": {"uri": Url::from_file_path(path).unwrap()},
                "position": {"line": position.0, "character": position.1},
            }),
        )
    }

    fn rename(&mut self, path: &Path, position: (u32, u32), new_name: &str) -> Value {
        self.result(
            "textDocument/rename",
            json!({
                "textDocument": {"uri": Url::from_file_path(path).unwrap()},
                "position": {"line": position.0, "character": position.1},
                "newName": new_name,
            }),
        )
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_reader(stdout: ChildStdout) -> Receiver<Value> {
    let (sender, receiver) = channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut content_length: Option<usize> = None;
            loop {
                let mut header = String::new();
                match reader.read_line(&mut header) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let trimmed = header.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    break;
                }
                if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                    content_length = value.trim().parse().ok();
                }
            }
            let Some(length) = content_length else {
                return;
            };
            let mut body = vec![0u8; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            match serde_json::from_slice::<Value>(&body) {
                Ok(message) => {
                    if sender.send(message).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    receiver
}

/// Position of the first byte of `needle` plus `offset`, as an LSP position.
fn position_of(source: &str, needle: &str, offset: usize) -> (u32, u32) {
    let start = source
        .find(needle)
        .unwrap_or_else(|| panic!("needle {needle:?} not found"));
    position_at(source, start + offset)
}

/// Position of the first byte of `needle`.
fn position(source: &str, needle: &str) -> (u32, u32) {
    position_of(source, needle, 0)
}

/// Position immediately after `needle` (a cursor at the end of a token).
fn position_after(source: &str, needle: &str) -> (u32, u32) {
    let start = source
        .find(needle)
        .unwrap_or_else(|| panic!("needle {needle:?} not found"));
    position_at(source, start + needle.len())
}

fn position_at(source: &str, byte_offset: usize) -> (u32, u32) {
    let before = &source[..byte_offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map(|index| index + 1).unwrap_or(0);
    let character = source[line_start..byte_offset].encode_utf16().count() as u32;
    (line, character)
}

fn labels(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("expected an array of completion items")
        .iter()
        .filter_map(|item| {
            item.get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn diagnostic_codes(diagnostics: &[Value]) -> Vec<String> {
    diagnostics
        .iter()
        .filter_map(|diagnostic| {
            diagnostic
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn uri_ends_with(value: &Value, suffix: &str) -> bool {
    value
        .get("uri")
        .and_then(Value::as_str)
        .is_some_and(|uri| uri.ends_with(suffix))
}

#[test]
fn route_definition_hover_and_references() {
    let fixture = Fixture::new("route");
    let mut lsp = fixture.start();
    let page = fixture.path("templates/page.html.twig");
    let text = fixture.text("templates/page.html.twig");
    let cursor = position(&text, "hello_world");

    let definition = lsp.definition(&page, cursor);
    assert!(
        uri_ends_with(&definition, "/config/routes.yaml"),
        "route definition should resolve to routes.yaml: {definition}"
    );
    assert_eq!(definition.pointer("/range/start/line"), Some(&json!(0)));

    let hover = lsp.hover(&page, cursor);
    let hover_text = hover
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        hover_text.contains("Symfony route `hello_world`"),
        "unexpected hover: {hover}"
    );
    assert!(
        hover_text.contains("/hello/{name}"),
        "hover should include the route path: {hover}"
    );

    let references = lsp.references(&page, cursor);
    let references = references.as_array().expect("references array");
    assert!(
        references
            .iter()
            .any(|location| uri_ends_with(location, "/config/routes.yaml")),
        "references should include the route declaration: {references:?}"
    );
    assert!(
        references
            .iter()
            .any(|location| uri_ends_with(location, "/templates/page.html.twig")),
        "references should include the Twig call site: {references:?}"
    );
}

#[test]
fn route_parameter_completion() {
    let fixture = Fixture::new("route-params");
    let mut lsp = fixture.start();
    let page = fixture.path("templates/page.html.twig");
    let text = fixture.text("templates/page.html.twig");
    let cursor = position_after(&text, "hello_world', ");

    let completion = lsp.completion(&page, cursor);
    assert!(
        labels(&completion).contains(&"name".to_string()),
        "route parameter completion should offer `name`: {completion}"
    );
}

#[test]
fn twig_template_definition_document_link_and_completion() {
    let fixture = Fixture::new("template");
    let mut lsp = fixture.start();

    let page = fixture.path("templates/page.html.twig");
    let page_text = fixture.text("templates/page.html.twig");
    let definition = lsp.definition(&page, position(&page_text, "base.html.twig"));
    assert!(
        uri_ends_with(&definition, "/templates/base.html.twig"),
        "extends should resolve to the parent template: {definition}"
    );

    let links = lsp.result(
        "textDocument/documentLink",
        json!({"textDocument": {"uri": Url::from_file_path(&page).unwrap()}}),
    );
    let links = links.as_array().expect("document link array");
    assert!(
        links.iter().any(|link| link
            .get("target")
            .and_then(Value::as_str)
            .is_some_and(|target| target.ends_with("/templates/base.html.twig"))),
        "document links should target the parent template: {links:?}"
    );

    let search = fixture.path("templates/search.html.twig");
    let search_text = fixture.text("templates/search.html.twig");
    let completion = lsp.completion(&search, position_after(&search_text, "include('pa"));
    assert!(
        labels(&completion).contains(&"page.html.twig".to_string()),
        "template completion should offer page.html.twig: {completion}"
    );
}

#[test]
fn twig_missing_template_diagnostic_and_create_action() {
    let fixture = Fixture::new("template-missing");
    let mut lsp = fixture.start();
    let page = fixture.path("templates/page.html.twig");
    lsp.open(&page, "twig");

    let diagnostics = lsp.diagnostics_for(&page);
    let codes = diagnostic_codes(&diagnostics);
    for expected in [
        "missing-template",
        "missing-translation",
        "missing-route-parameter",
    ] {
        assert!(
            codes.contains(&expected.to_string()),
            "expected a {expected} diagnostic: {diagnostics:?}"
        );
    }

    let actions = lsp.code_action(&page, 6, &diagnostics);
    let actions = actions.as_array().expect("code action array");
    let create = actions
        .iter()
        .find(|action| {
            action
                .get("title")
                .and_then(Value::as_str)
                .is_some_and(|title| title.contains("Create Twig template `missing.html.twig`"))
        })
        .unwrap_or_else(|| panic!("expected a create-template action: {actions:?}"));
    let operations = create
        .pointer("/edit/documentChanges")
        .and_then(Value::as_array)
        .expect("create action should carry document changes");
    assert!(
        operations.iter().any(|operation| operation
            .get("uri")
            .and_then(Value::as_str)
            .is_some_and(|uri| uri.ends_with("/templates/missing.html.twig"))),
        "create action should create the missing template file: {operations:?}"
    );
}

#[test]
fn twig_rename_updates_references_and_renames_file() {
    let fixture = Fixture::new("template-rename");
    let mut lsp = fixture.start();
    let page = fixture.path("templates/page.html.twig");
    let text = fixture.text("templates/page.html.twig");
    let cursor = position(&text, "base.html.twig");

    let prepared = lsp.prepare_rename(&page, cursor);
    assert_eq!(
        prepared.get("placeholder").and_then(Value::as_str),
        Some("base.html.twig"),
        "prepareRename should offer the template name: {prepared}"
    );

    let edit = lsp.rename(&page, cursor, "layout.html.twig");
    let operations = edit
        .pointer("/documentChanges")
        .and_then(Value::as_array)
        .expect("rename should return document changes");
    assert!(
        operations.iter().any(|operation| {
            operation
                .get("oldUri")
                .and_then(Value::as_str)
                .is_some_and(|uri| uri.ends_with("/templates/base.html.twig"))
                && operation
                    .get("newUri")
                    .and_then(Value::as_str)
                    .is_some_and(|uri| uri.ends_with("/templates/layout.html.twig"))
        }),
        "rename should move the template file: {operations:?}"
    );
    assert!(
        operations.iter().any(|operation| {
            operation
                .pointer("/textDocument/uri")
                .and_then(Value::as_str)
                .is_some_and(|uri| uri.ends_with("/templates/page.html.twig"))
                && operation
                    .pointer("/edits/0/newText")
                    .and_then(Value::as_str)
                    == Some("layout.html.twig")
        }),
        "rename should update the Twig reference: {operations:?}"
    );
}

#[test]
fn environment_variable_hover_definition_and_completion() {
    let fixture = Fixture::new("env");
    let mut lsp = fixture.start();

    let controller = fixture.path("src/Controller/HelloController.php");
    let controller_text = fixture.text("src/Controller/HelloController.php");
    let cursor = position_after(&controller_text, "env('");

    let definition = lsp.definition(&controller, cursor);
    assert!(
        uri_ends_with(&definition, "/.env"),
        "env definition should resolve to the .env file: {definition}"
    );

    let hover = lsp.hover(&controller, cursor);
    let hover_text = hover
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        hover_text.contains("Environment variable `APP_ENV`"),
        "unexpected env hover: {hover}"
    );

    let config = fixture.path("src/Config.php");
    let config_text = fixture.text("src/Config.php");
    let completion = lsp.completion(&config, position_after(&config_text, "env('APP"));
    assert!(
        labels(&completion).contains(&"APP_ENV".to_string()),
        "env completion should offer APP_ENV: {completion}"
    );
}

#[test]
fn translation_hover_definition_and_completion() {
    let fixture = Fixture::new("translation");
    let mut lsp = fixture.start();

    let page = fixture.path("templates/page.html.twig");
    let page_text = fixture.text("templates/page.html.twig");
    let cursor = position(&page_text, "greeting");

    let definition = lsp.definition(&page, cursor);
    assert!(
        uri_ends_with(&definition, "/translations/messages.en.yaml"),
        "translation definition should resolve to the catalog: {definition}"
    );

    let hover = lsp.hover(&page, cursor);
    let hover_text = hover
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        hover_text.contains("Translation key `greeting`")
            && hover_text.contains("domain `messages`"),
        "unexpected translation hover: {hover}"
    );

    let trans = fixture.path("templates/trans.html.twig");
    let trans_text = fixture.text("templates/trans.html.twig");
    let completion = lsp.completion(&trans, position_after(&trans_text, "'gre"));
    assert!(
        labels(&completion).contains(&"greeting".to_string()),
        "translation completion should offer greeting: {completion}"
    );
}

#[test]
fn service_hover_definition_and_completion() {
    let fixture = Fixture::new("service");
    let mut lsp = fixture.start();

    let services = fixture.path("config/services.yaml");
    let services_text = fixture.text("config/services.yaml");
    let cursor = position_after(&services_text, "'@");

    let definition = lsp.definition(&services, cursor);
    assert!(
        uri_ends_with(&definition, "/config/services.yaml"),
        "service definition should resolve within services.yaml: {definition}"
    );
    assert_eq!(
        definition.pointer("/range/start/line"),
        Some(&json!(1)),
        "service definition should point at the declaration line: {definition}"
    );

    let hover = lsp.hover(&services, cursor);
    let hover_text = hover
        .pointer("/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        hover_text.contains("Symfony service `mailer.transport`"),
        "unexpected service hover: {hover}"
    );

    let extra = fixture.path("config/services.extra.yaml");
    let extra_text = fixture.text("config/services.extra.yaml");
    let completion = lsp.completion(&extra, position_after(&extra_text, "'@mail"));
    assert!(
        labels(&completion).contains(&"mailer.transport".to_string()),
        "service completion should offer mailer.transport: {completion}"
    );
}

#[test]
fn service_missing_diagnostic_and_tag_action() {
    let fixture = Fixture::new("service-missing");
    let mut lsp = fixture.start();
    let services = fixture.path("config/services.yaml");
    lsp.open(&services, "yaml");

    let diagnostics = lsp.diagnostics_for(&services);
    assert!(
        diagnostic_codes(&diagnostics).contains(&"missing-service".to_string()),
        "expected a missing-service diagnostic: {diagnostics:?}"
    );

    let actions = lsp.code_action(&services, 7, &diagnostics);
    let actions = actions.as_array().expect("code action array");
    assert!(
        actions.iter().any(|action| action
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| title.contains("kernel.event_subscriber"))),
        "expected an event-subscriber tag action: {actions:?}"
    );
}

#[test]
fn php_missing_route_and_environment_diagnostics() {
    let fixture = Fixture::new("php-diagnostics");
    let mut lsp = fixture.start();
    let controller = fixture.path("src/Controller/HelloController.php");
    lsp.open(&controller, "php");

    let codes = diagnostic_codes(&lsp.diagnostics_for(&controller));
    assert!(
        codes.contains(&"missing-route".to_string()),
        "expected a missing-route diagnostic: {codes:?}"
    );
    assert!(
        codes.contains(&"missing-environment-variable".to_string()),
        "expected a missing-environment-variable diagnostic: {codes:?}"
    );
}

#[test]
fn route_attribute_code_action() {
    let fixture = Fixture::new("route-action");
    let mut lsp = fixture.start();
    let controller = fixture.path("src/Controller/HelloController.php");

    let actions = lsp.code_action(&controller, 13, &[]);
    let actions = actions.as_array().expect("code action array");
    assert!(
        actions.iter().any(|action| action
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| title.contains("Add route attribute `hello.preview`"))),
        "expected a route attribute action: {actions:?}"
    );
}

#[test]
fn constructor_injection_code_action() {
    let fixture = Fixture::new("inject-action");
    let mut lsp = fixture.start();
    let controller = fixture.path("src/Controller/ReportController.php");

    let actions = lsp.code_action(&controller, 7, &[]);
    let actions = actions.as_array().expect("code action array");
    assert!(
        actions.iter().any(|action| action
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| title.contains("Inject `Mailer` through the constructor"))),
        "expected a constructor injection action: {actions:?}"
    );
}

#[test]
fn watched_file_registration_and_index_updates() {
    let fixture = Fixture::new("watched");
    let mut lsp = Lsp::spawn();
    lsp.initialize_with_capabilities(
        &fixture.root,
        json!({"workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}}}),
    );

    // With dynamic registration advertised, the server must register watchers.
    let registration = lsp.wait_for("client/registerCapability", |message| {
        message.get("method").and_then(Value::as_str) == Some("client/registerCapability")
    });
    let registrations = registration
        .pointer("/params/registrations")
        .and_then(Value::as_array)
        .expect("registerCapability should carry registrations");
    let watchers = registrations
        .iter()
        .find(|registration| {
            registration.get("method").and_then(Value::as_str)
                == Some("workspace/didChangeWatchedFiles")
        })
        .and_then(|registration| registration.pointer("/registerOptions/watchers"))
        .and_then(Value::as_array)
        .expect("the watched-file registration should list watchers");
    assert!(
        watchers
            .iter()
            .any(|watcher| watcher.get("globPattern").and_then(Value::as_str) == Some("**/*.twig")),
        "watchers should include Twig templates: {watchers:?}"
    );

    let page = fixture.path("templates/page.html.twig");
    lsp.open(&page, "twig");
    let codes = diagnostic_codes(&lsp.diagnostics_for(&page));
    assert!(
        codes.contains(&"missing-template".to_string()),
        "the missing template should warn before it exists: {codes:?}"
    );
    assert!(
        !codes.contains(&"missing-route".to_string()),
        "the existing route should not warn: {codes:?}"
    );

    // Creating the referenced template on disk and notifying the server clears it.
    fixture.write(
        "templates/missing.html.twig",
        "{% extends 'base.html.twig' %}\n",
    );
    lsp.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes": [{
            "uri": Url::from_file_path(fixture.path("templates/missing.html.twig")).unwrap(),
            "type": 1,
        }]}),
    );
    let codes = diagnostic_codes(&lsp.diagnostics_for(&page));
    assert!(
        !codes.contains(&"missing-template".to_string()),
        "a created template should clear the warning: {codes:?}"
    );

    // Deleting the route configuration makes the existing reference unresolved.
    fixture.remove("config/routes.yaml");
    lsp.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes": [{
            "uri": Url::from_file_path(fixture.path("config/routes.yaml")).unwrap(),
            "type": 3,
        }]}),
    );
    let codes = diagnostic_codes(&lsp.diagnostics_for(&page));
    assert!(
        codes.contains(&"missing-route".to_string()),
        "a deleted route file should surface a missing-route warning: {codes:?}"
    );
}

#[test]
fn rename_declines_ordinary_php_symbols() {
    // The documented rename policy relies on Symfony returning no result for
    // ordinary PHP symbols so it never produces a partial edit when it is the
    // PHP rename provider. Framework strings are covered by the template and
    // route rename tests.
    let fixture = Fixture::new("rename-declines");
    let mut lsp = fixture.start();
    let controller = fixture.path("src/Controller/HelloController.php");
    let text = fixture.text("src/Controller/HelloController.php");
    let cursor = position(&text, "HelloController");

    let prepared = lsp.prepare_rename(&controller, cursor);
    assert_eq!(
        prepared,
        Value::Null,
        "prepareRename should decline a PHP class symbol: {prepared}"
    );
    let edit = lsp.rename(&controller, cursor, "RenamedController");
    assert_eq!(
        edit,
        Value::Null,
        "rename should decline a PHP class symbol: {edit}"
    );
}
