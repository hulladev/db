use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Worker {
    child: Child,
    bundle: PathBuf,
}
impl Worker {
    fn start(body: &str) -> Self {
        Self::with_invocation(body, json!({}))
    }
    fn with_invocation(body: &str, invocation: Value) -> Self {
        let bundle = std::env::temp_dir().join(format!(
            "hulla-js-{}-{}.js",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&bundle, body).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_hulladb-js"))
            .arg(&bundle)
            .env("HULLA_ENGINE_TEST_SECRET", "must-not-be-visible")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.as_mut().unwrap(), "{invocation}").unwrap();
        Self { child, bundle }
    }
    fn finish(mut self) -> Value {
        let started = Instant::now();
        loop {
            if self.child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(9),
                "worker exceeded bounded test deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut output = String::new();
        use std::io::Read;
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        let mut error = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        assert!(
            self.child.wait().unwrap().success(),
            "worker failed: {error}"
        );
        assert!(error.is_empty(), "worker leaked diagnostics: {error}");
        serde_json::from_str(output.trim())
            .unwrap_or_else(|_| panic!("unexpected worker output: {output}"))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.bundle);
    }
}
fn run(body: &str) -> Value {
    Worker::start(body).finish()
}

#[test]
fn standard_computation_works_without_host_capabilities_or_dynamic_code() {
    let result = run(r#"
        globalThis.__hullaExecute = async () => {
            const absent = ['process','Bun','Deno','fetch','XMLHttpRequest','WebSocket','require','console','performance','setTimeout','crypto','SharedArrayBuffer','Atomics','WeakRef','FinalizationRegistry','WebAssembly'];
            for (const name of absent) if (typeof globalThis[name] !== 'undefined') throw Error('Unexpected host ' + name);
            const blocked = [() => eval('1'), () => Function('return 1')(), () => (async () => {}).constructor('return 1')(), () => (function*(){}).constructor('yield 1')(), () => (async function*(){}).constructor('yield 1')(), () => Math.random(), () => Date.now(), () => new Date(), () => Date(), () => new Date('2026-01-01'), () => new Date(0).getHours()];
            for (const attempt of blocked) { let denied = false; try { attempt(); } catch { denied = true; } if (!denied) throw Error('Unexpected capability'); }
            let deniedImport = false; try { await import('node:fs'); } catch { deniedImport = true; }
            if (!deniedImport) throw Error('Unexpected import');
            const map = new Map([['value', 4]]);
            const value = await Promise.resolve(map.get('value'));
            return JSON.stringify({ value, timestamp: new Date('2026-01-01T00:00:00Z').toISOString(), matches: /ok/.test('okay') });
        };
    "#);
    assert_eq!(
        result,
        json!({"type":"result", "result":{"value":4,"timestamp":"2026-01-01T00:00:00.000Z","matches":true}})
    );
}

#[test]
fn database_messages_are_correlated_and_json_only() {
    let mut worker = Worker::start(
        r#"globalThis.__hullaExecute = async () => {
        const result = JSON.parse(await __hullaDb(JSON.stringify({method:'select',table:'tasks'})));
        return JSON.stringify(result);
    };"#,
    );
    let mut output = BufReader::new(worker.child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"type":"operation","id":1,"operation":{"method":"select","table":"tasks"}})
    );
    writeln!(
        worker.child.stdin.as_mut().unwrap(),
        "{}",
        json!({"id":1,"result":{"ok":[{"title":"native"}]}})
    )
    .unwrap();
    worker.child.stdout = Some(output.into_inner());
    assert_eq!(
        worker.finish(),
        json!({"type":"result", "result":{"ok":[{"title":"native"}]}})
    );
}

#[test]
fn pending_work_and_unhandled_failures_cannot_publish_results() {
    for source in [
        "globalThis.__hullaExecute = async () => { __hullaDb('{}'); return '1'; };",
        "globalThis.__hullaExecute = async () => { Promise.resolve().then(() => __hullaDb('{}')); return '1'; };",
        "globalThis.__hullaExecute = async () => { Promise.reject(Error('secret')); return '1'; };",
        "globalThis.__hullaExecute = async () => { throw Error('secret'); };",
        "await __hullaDb('{}'); globalThis.__hullaExecute = async () => '1';",
    ] {
        assert_eq!(
            run(source),
            json!({"type":"error","code":"handler_failed"}),
            "{source}"
        );
    }
}

#[test]
fn mismatched_database_responses_fail_closed() {
    let mut worker =
        Worker::start("globalThis.__hullaExecute = async () => await __hullaDb('{}');");
    let mut output = BufReader::new(worker.child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    writeln!(
        worker.child.stdin.as_mut().unwrap(),
        "{{\"id\":99,\"result\":null}}"
    )
    .unwrap();
    worker.child.stdout = Some(output.into_inner());
    assert_eq!(
        worker.finish(),
        json!({"type":"error","code":"handler_failed"})
    );
}

#[test]
fn cpu_deadline_interrupts_loops_and_promise_chains() {
    for source in [
        "globalThis.__hullaExecute = async () => { while(true) {} };",
        "globalThis.__hullaExecute = async () => { while(true) await Promise.resolve(); };",
    ] {
        let started = Instant::now();
        assert_eq!(
            run(source),
            json!({"type":"error","code":"execution_limit"})
        );
        assert!(started.elapsed() < Duration::from_secs(8));
    }
}

#[test]
fn regexp_execution_is_bounded() {
    let started = Instant::now();
    let result = run(
        "globalThis.__hullaExecute = async () => JSON.stringify(/^(a+)+$/.test('a'.repeat(40) + '!')); ",
    );
    assert!(
        result == json!({"type":"error","code":"execution_limit"})
            || result == json!({"type":"result","result":false})
    );
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
fn memory_and_stack_exhaustion_fail_without_host_crash() {
    for body in [
        "const rows=[]; while(true) rows.push(new Array(100000).fill(123));",
        "function recurse(){return recurse()+1} recurse();",
    ] {
        let result = run(&format!(
            "globalThis.__hullaExecute = async () => {{ {body} return 'null'; }};"
        ));
        assert_eq!(result["type"], "error");
        assert!(matches!(
            result["code"].as_str(),
            Some("handler_failed" | "execution_limit")
        ));
    }
}

#[test]
fn terminal_rejections_keep_only_allowlisted_protocol_codes() {
    for code in ["schema_mismatch", "invalid_request", "handler_failed"] {
        let source =
            format!("globalThis.__hullaExecute = async () => {{ throw Error('{code}'); }};");
        assert_eq!(run(&source), json!({"type":"error","code":code}));
    }
}

#[test]
fn trusted_invocation_budgets_change_cpu_and_heap_enforcement() {
    let started = Instant::now();
    assert_eq!(
        Worker::with_invocation(
            "globalThis.__hullaExecute=async()=>{while(true){}}",
            json!({"limits":{"executionMs":50}})
        )
        .finish(),
        json!({"type":"error","code":"execution_limit"})
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    let source = "globalThis.__hullaExecute=async()=>{const values=new Array(262144).fill(1);return JSON.stringify(values.length)}";
    assert_eq!(
        Worker::with_invocation(source, json!({"limits":{"memoryBytes":1024*1024}})).finish()["type"],
        "error"
    );
    assert_eq!(
        Worker::with_invocation(source, json!({"limits":{"memoryBytes":16*1024*1024}})).finish(),
        json!({"type":"result","result":262144})
    );
}
