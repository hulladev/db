//! A fresh restricted JavaScript realm for one database declaration invocation.
//! Only bounded JSON database messages cross the application/host boundary.
use rquickjs::{
    Context, Ctx, Exception, Function, Module, Promise, Runtime, Value, WriteOptions,
    context::intrinsic, promise::PromiseState,
};
use serde_json::{Value as Json, json};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    io::{self, BufRead, Read, Write},
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

const MEMORY_BYTES: usize = 64 * 1024 * 1024;
const STACK_BYTES: usize = 512 * 1024;
const FRAME_BYTES: usize = 66 * 1024 * 1024;
const MAX_PENDING: usize = 32;
const DEADLINE: Duration = Duration::from_secs(5);

type Failure = &'static str;
type Outcome<T> = Result<T, Failure>;

fn runtime(deadline: Instant, memory: usize, stack: usize) -> Outcome<Runtime> {
    let runtime = Runtime::new().map_err(|_| "handler_failed")?;
    runtime.set_memory_limit(memory);
    runtime.set_max_stack_size(stack);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    Ok(runtime)
}

/// Bytecode never comes from a file or a caller: Module::declare + write in
/// this process produced it with
/// exactly the same linked engine, so its format and contents are valid.
#[allow(unsafe_code)]
fn load_trusted<'js>(ctx: Ctx<'js>, bytes: &[u8]) -> rquickjs::Result<Module<'js>> {
    // SAFETY: callers exclusively pass the output of compile() below.
    unsafe { Module::load(ctx, bytes) }
}

fn compile(
    source: &str,
    name: &str,
    deadline: Instant,
    memory: usize,
    stack: usize,
) -> Outcome<Vec<u8>> {
    let runtime = runtime(deadline, memory, stack)?;
    let context = Context::full(&runtime).map_err(|_| "handler_failed")?;
    context
        .with(|ctx| {
            Module::declare(ctx, name, source)
                .and_then(|module| module.write(WriteOptions::default()))
        })
        .map_err(|_| {
            if Instant::now() >= deadline {
                "execution_limit"
            } else {
                "handler_failed"
            }
        })
}

fn send(frame: &Json) -> Outcome<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, frame).map_err(|_| "handler_failed")?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(|_| "handler_failed")
}

fn receive(receiver: &mpsc::Receiver<Outcome<Json>>, deadline: Instant) -> Outcome<Json> {
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => "execution_limit",
            mpsc::RecvTimeoutError::Disconnected => "handler_failed",
        })?
}

fn input() -> mpsc::Receiver<Outcome<Json>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            let mut frame = Vec::new();
            let result = (&mut input)
                .take((FRAME_BYTES + 1) as u64)
                .read_until(b'\n', &mut frame);
            let valid =
                result.is_ok() && frame.last() == Some(&b'\n') && frame.len() <= FRAME_BYTES;
            let value = if valid {
                serde_json::from_slice(&frame).map_err(|_| "invalid_request")
            } else {
                Err("invalid_request")
            };
            let stop = value.is_err();
            if sender.send(value).is_err() || stop {
                break;
            }
        }
    });
    receiver
}

struct Request<'js> {
    operation: Json,
    bytes: usize,
    resolve: Function<'js>,
}

fn failure(ctx: &Ctx<'_>) -> Failure {
    let thrown = ctx.catch();
    let message = thrown
        .as_object()
        .and_then(|object| object.get::<_, String>("message").ok());
    match message.as_deref() {
        Some("schema_mismatch") => "schema_mismatch",
        Some("invalid_request") => "invalid_request",
        Some("row_expectation_failed") => "row_expectation_failed",
        _ => "handler_failed",
    }
}

enum ExecutionResult {
    Value(Json),
    ApplicationError(Json),
}

fn run(path: &str) -> Outcome<ExecutionResult> {
    let started = Instant::now();
    let receiver = input();
    let invocation = receive(&receiver, started + DEADLINE)?;
    let setting = |name: &str, default: usize, maximum: usize| -> Outcome<usize> {
        let value = match invocation.get("limits").and_then(|limits| limits.get(name)) {
            None => default,
            Some(value) => usize::try_from(value.as_u64().ok_or("invalid_request")?)
                .map_err(|_| "invalid_request")?,
        };
        if value == 0 || value > maximum {
            Err("invalid_request")
        } else {
            Ok(value)
        }
    };
    let memory = setting("memoryBytes", MEMORY_BYTES, 1024 * 1024 * 1024)?;
    let stack = setting("stackBytes", STACK_BYTES, 16 * 1024 * 1024)?;
    let execution_ms = setting("executionMs", 5000, 60_000)?;
    let frame_bytes = setting("inputBytes", 16 * 1024 * 1024, 64 * 1024 * 1024)?
        .max(setting("outputBytes", 16 * 1024 * 1024, 64 * 1024 * 1024)?)
        .max(setting("readBytes", 64 * 1024 * 1024, 64 * 1024 * 1024)?)
        + 2 * 1024 * 1024;
    let deadline = started + Duration::from_millis(execution_ms as u64);
    // The CLI supplies a trusted locally generated bundle, never a browser path.
    let mut source = String::new();
    std::fs::File::open(path)
        .map_err(|_| "handler_failed")?
        .take((FRAME_BYTES + 1) as u64)
        .read_to_string(&mut source)
        .map_err(|_| "handler_failed")?;
    if source.len() > FRAME_BYTES {
        return Err("execution_limit");
    }
    let bootstrap = compile(
        include_str!("bootstrap.js"),
        "hulla-bootstrap",
        deadline,
        memory,
        stack,
    )?;
    let application = compile(&source, "hulla-application", deadline, memory, stack)?;
    let runtime = runtime(deadline, memory, stack)?;
    let unhandled = Rc::new(Cell::new(0usize));
    let tracker = unhandled.clone();
    runtime.set_host_promise_rejection_tracker(Some(Box::new(move |_, _, _, handled| {
        tracker.set(if handled {
            tracker.get().saturating_sub(1)
        } else {
            tracker.get() + 1
        });
    })));
    // No Eval, performance clock, weak finalizers, typed/shared buffers or loader.
    let context = Context::custom::<(
        intrinsic::Date,
        intrinsic::RegExpCompiler,
        intrinsic::RegExp,
        intrinsic::Json,
        intrinsic::Proxy,
        intrinsic::MapSet,
        intrinsic::Promise,
    )>(&runtime)
    .map_err(|_| "handler_failed")?;
    let result = context.with(|ctx| {
        execute(
            ctx,
            &bootstrap,
            &application,
            invocation,
            Execution {
                receiver: &receiver,
                deadline,
                unhandled: &unhandled,
                frame_bytes,
            },
        )
    });
    if Instant::now() >= deadline {
        Err("execution_limit")
    } else {
        result
    }
}

#[allow(unsafe_code)]
fn jobs_pending(ctx: &Ctx<'_>) -> bool {
    // SAFETY: ctx holds the live runtime lock; this only inspects its job queue.
    unsafe { rquickjs::qjs::JS_IsJobPending(rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr())) }
}

#[allow(unsafe_code)]
fn execute_job(ctx: &Ctx<'_>) -> Outcome<bool> {
    // SAFETY: ctx holds the runtime lock and this runtime contains one context.
    // Unlike Ctx::execute_pending_job this preserves job exceptions as failures.
    let result = unsafe {
        let mut job_context = std::ptr::null_mut();
        rquickjs::qjs::JS_ExecutePendingJob(
            rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()),
            &mut job_context,
        )
    };
    if result < 0 {
        Err("handler_failed")
    } else {
        Ok(result != 0)
    }
}

struct QueueCleanup<'js>(Rc<RefCell<VecDeque<Request<'js>>>>);
impl Drop for QueueCleanup<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().clear();
    }
}

struct Execution<'a> {
    receiver: &'a mpsc::Receiver<Outcome<Json>>,
    deadline: Instant,
    unhandled: &'a Cell<usize>,
    frame_bytes: usize,
}

fn execute<'js>(
    ctx: Ctx<'js>,
    bootstrap: &[u8],
    application: &[u8],
    invocation: Json,
    execution: Execution<'_>,
) -> Outcome<ExecutionResult> {
    let Execution {
        receiver,
        deadline,
        unhandled,
        frame_bytes,
    } = execution;
    let requests = Rc::new(RefCell::new(VecDeque::<Request<'js>>::new()));
    let _cleanup = QueueCleanup(requests.clone());
    let bytes = Rc::new(Cell::new(0usize));
    let closed = Rc::new(Cell::new(true));
    let queue = requests.clone();
    let queued_bytes = bytes.clone();
    let capability_closed = closed.clone();
    let host = Function::new(ctx.clone(), move |ctx: Ctx<'js>, operation: String| {
        if capability_closed.get() || Instant::now() >= deadline {
            return Err(Exception::throw_type(&ctx, "Database capability is closed"));
        }
        if operation.len() > frame_bytes
            || queue.borrow().len() >= MAX_PENDING
            || queued_bytes.get() + operation.len() > frame_bytes
        {
            return Err(Exception::throw_type(
                &ctx,
                "Database request budget exceeded",
            ));
        }
        let parsed: Json = serde_json::from_str(&operation)
            .map_err(|_| Exception::throw_type(&ctx, "Invalid database request"))?;
        if !parsed.is_object() {
            return Err(Exception::throw_type(&ctx, "Invalid database request"));
        }
        let (promise, resolve, _) = Promise::new(&ctx)?;
        queued_bytes.set(queued_bytes.get() + operation.len());
        queue.borrow_mut().push_back(Request {
            operation: parsed,
            bytes: operation.len(),
            resolve,
        });
        Ok(promise)
    })
    .map_err(|_| "handler_failed")?;
    ctx.globals()
        .set("__hullaDb", host)
        .map_err(|_| "handler_failed")?;
    for bytecode in [bootstrap, application] {
        let (_, initialized) = load_trusted(ctx.clone(), bytecode)
            .and_then(Module::eval)
            .map_err(|_| failure(&ctx))?;
        initialized.finish::<()>().map_err(|_| failure(&ctx))?;
    }
    if jobs_pending(&ctx) || unhandled.get() != 0 {
        return Err("handler_failed");
    }
    let entry: Function = ctx
        .globals()
        .get("__hullaExecute")
        .map_err(|_| "handler_failed")?;
    let input: Value = ctx
        .json_parse(invocation.to_string())
        .map_err(|_| "invalid_request")?;
    closed.set(false);
    let promise: Promise = entry.call((input,)).map_err(|_| failure(&ctx))?;
    let mut id = 0u64;
    while promise.state() == PromiseState::Pending {
        if Instant::now() >= deadline {
            return Err("execution_limit");
        }
        if execute_job(&ctx)? {
            continue;
        }
        let request = requests.borrow_mut().pop_front();
        let Some(request) = request else {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        };
        bytes.set(bytes.get() - request.bytes);
        id += 1;
        send(&json!({"type":"operation","id":id,"operation":request.operation}))?;
        let response = receive(receiver, deadline)?;
        if response.get("id").and_then(Json::as_u64) != Some(id) || response.get("result").is_none()
        {
            return Err("handler_failed");
        }
        request
            .resolve
            .call::<_, ()>((response["result"].to_string(),))
            .map_err(|_| failure(&ctx))?;
    }
    closed.set(true);
    // Read a rejected invocation before checking detached rejections: QuickJS
    // also tracks the invocation promise itself as unhandled by JavaScript.
    // This only classifies failure; all successful results still pass the fence.
    let serialized: String = match promise.result().ok_or("handler_failed")? {
        Ok(value) => value,
        Err(_) => {
            let thrown = ctx.catch();
            if let Some(payload) = thrown
                .as_object()
                .and_then(|value| value.get::<_, String>("application").ok())
            {
                if payload.len() > 16 * 1024 {
                    return Err("execution_limit");
                }
                let value = serde_json::from_str(&payload).map_err(|_| "handler_failed")?;
                return Ok(ExecutionResult::ApplicationError(value));
            }
            let code = thrown
                .as_object()
                .and_then(|value| value.get::<_, String>("message").ok());
            return Err(match code.as_deref() {
                Some("schema_mismatch") => "schema_mismatch",
                Some("invalid_request") => "invalid_request",
                _ => "handler_failed",
            });
        }
    };
    if !requests.borrow().is_empty() || jobs_pending(&ctx) || unhandled.get() != 0 {
        return Err("handler_failed");
    }
    if serialized.len() > frame_bytes {
        return Err("execution_limit");
    }
    let result = serde_json::from_str(&serialized).map_err(|_| "handler_failed")?;
    if Instant::now() >= deadline {
        return Err("execution_limit");
    }
    Ok(ExecutionResult::Value(result))
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let outcome = if args.len() == 2 {
        run(&args[1])
    } else {
        Err("invalid_request")
    };
    let frame = match outcome {
        Ok(ExecutionResult::Value(result)) => json!({"type":"result", "result":result}),
        Ok(ExecutionResult::ApplicationError(error)) => {
            json!({"type":"application_error", "error":error})
        }
        Err(code) => json!({"type":"error", "code":code}),
    };
    if send(&frame).is_err() {
        std::process::exit(1);
    }
}
