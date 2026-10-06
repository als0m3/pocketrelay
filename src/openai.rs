use crate::{
    backend::{self, Event, Output, Prompt},
    config::{self, id, now, text},
    error::{Error, Result},
    prompt::{self, Conversation, Filter},
    App,
};
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{
    convert::Infallible,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
#[derive(Clone)]
pub struct Model {
    pub public: String,
    pub plain: String,
    pub provider: String,
    pub pinned: Option<Value>,
}
pub async fn resolve(app: &Arc<App>, input: &str) -> Result<Model> {
    let mut m = input.trim().to_string();
    for prefix in [
        "anthropic/",
        "claude-code/",
        "claude/",
        "openai/",
        "google/",
        "antigravity/",
    ] {
        if let Some(s) = m.strip_prefix(prefix) {
            m = s.into();
        }
    }
    let mut pinned = None;
    if let Some((account, plain)) = m.split_once('/') {
        if let Ok(a) = app.store.lock().unwrap().account(account) {
            pinned = Some(a);
            m = plain.into();
        }
    }
    if m.is_empty() && !app.config.require_account {
        m = config::envs("REMOTE_OAI_MODEL", "sonnet");
    }
    let provider = if m.starts_with("claude")
        || ["opus", "sonnet", "haiku", "fable", "default", "opusplan"]
            .contains(&m.split('[').next().unwrap_or(""))
    {
        "claude"
    } else if ["gemini", "gemma", "gpt-oss"]
        .iter()
        .any(|p| m.starts_with(p))
        || app.catalog.lock().unwrap()["antigravity"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| text(v, "id") == m))
    {
        "antigravity"
    } else {
        "codex"
    };
    let mut provider = provider.to_string();
    if provider == "codex" {
        app.refresh_catalog(false).await;
        let models = app.catalog.lock().unwrap()["codex"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let name = m.strip_prefix("codex/").unwrap_or(&m);
        if let Some(found) = models.iter().find(|v| text(v, "id") == name) {
            m = text(found, "id").into();
        } else if app.config.require_account {
            return Err(
                Error::new(400, format!("Unknown model: {m}. See /v1/models."))
                    .code("model_not_found")
                    .param("model"),
            );
        } else if ["codex", "gpt", "o1", "o3", "o4", "chatgpt"]
            .iter()
            .any(|p| m.starts_with(p))
            && !models.is_empty()
        {
            m = text(
                models
                    .iter()
                    .find(|v| v["isDefault"] == true)
                    .unwrap_or(&models[0]),
                "id",
            )
            .into();
        } else {
            provider = "claude".into();
            m = config::envs("REMOTE_OAI_MODEL", "sonnet");
        }
    }
    if app.config.require_account && pinned.is_none() {
        return Err(Error::new(
            400,
            format!("Specify an account: <account>/{m}. See /v1/models."),
        )
        .code("account_required")
        .param("model"));
    }
    if let Some(a) = &pinned {
        if text(a, "provider") != provider {
            return Err(Error::new(400, "The model does not belong to this account").param("model"));
        }
        if a["enabled"] != true {
            return Err(Error::new(503, "Account disabled").code("account_disabled"));
        }
    }
    if (provider == "codex" && app.config.codex.is_empty())
        || (provider == "antigravity" && app.config.antigravity.is_empty())
    {
        return Err(Error::new(503, "Provider CLI is missing").code("provider_unavailable"));
    }
    let public = pinned
        .as_ref()
        .map(|a| format!("{}/{}", text(a, "slug"), m))
        .unwrap_or_else(|| m.clone());
    Ok(Model {
        public,
        plain: m,
        provider,
        pinned,
    })
}
pub fn run(app: Arc<App>, model: Model, mut p: Prompt) -> Output {
    p.model = model.plain.clone();
    Box::pin(async_stream::try_stream! {
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(app.config.request_timeout);
        let mut tried = vec![];
        let mut last = None;
        loop {
            let account = {
                let store = app.store.lock().unwrap();
                if let Some(a) = &model.pinned {
                    if tried.contains(&text(a, "id").to_string()) {
                        None
                    } else {
                        Some(a.clone())
                    }
                } else {
                    store
                        .accounts
                        .iter()
                        .find(|a| {
                            text(a, "provider") == model.provider
                                && store.available(a)
                                && !tried.contains(&text(a, "id").into())
                        })
                        .cloned()
                }
            };
            let account = account.ok_or_else(|| {
                last.clone().unwrap_or_else(|| {
                    Error::new(
                        503,
                        format!(
                            "No {} account available. Add one in /admin.",
                            model.provider
                        ),
                    )
                    .code("no_account_available")
                })
            })?;
            let aid = text(&account, "id").to_owned();
            tried.push(aid.clone());
            let queue = Duration::from_secs(config::number("REMOTE_QUEUE_TIMEOUT", 120));
            let permit = tokio::time::timeout_at(
                deadline.min(tokio::time::Instant::now() + queue),
                app.sem.clone().acquire_owned(),
            )
            .await
            .map_err(|_| {
                Error::new(503, "Server busy; try again shortly").code("server_busy")
            })?
            .map_err(|_| Error::new(503, "Server is shutting down"))?;
            let mut stream = backend::stream(app.clone(), account.clone(), p.clone());
            let mut started = false;
            let mut output_bytes = 0usize;
            let mut error = None;
            loop {
                match tokio::time::timeout_at(deadline, stream.next()).await {
                    Err(_) => {
                        error = Some(Error::new(
                            504,
                            "The provider did not finish within the time limit",
                        ));
                        break;
                    }
                    Ok(Some(Err(e))) => {
                        error = Some(e);
                        break;
                    }
                    Ok(Some(Ok(Event::Text(t)))) => {
                        started = true;
                        output_bytes += t.len();
                        if output_bytes
                            > config::number("REMOTE_MAX_OUTPUT_MB", 16) as usize * 1024 * 1024
                        {
                            error =
                                Some(Error::new(502, "Provider response is too large"));
                            break;
                        }
                        yield Event::Text(t);
                    }
                    Ok(Some(Ok(Event::Done(mut info)))) => {
                        info["account"] = account["label"].clone();
                        app.requests.fetch_add(1, Ordering::Relaxed);
                        app.output_tokens.fetch_add(
                            info["usage"]["output_tokens"].as_u64().unwrap_or(0),
                            Ordering::Relaxed,
                        );
                        app.store.lock().unwrap().success(&aid);
                        yield Event::Done(info);
                        break;
                    }
                    Ok(None) => {
                        error = Some(Error::new(502, "Stream interrupted before the result"));
                        break;
                    }
                }
            }
            drop(stream);
            drop(permit);
            if let Some(e) = error {
                app.errors.fetch_add(1, Ordering::Relaxed);
                let retry = app.store.lock().unwrap().failure(&aid, &e);
                if started || !retry || model.pinned.is_some() {
                    Err(e.clone())?;
                }
                last = Some(e);
            } else {
                break;
            }
        }
    })
}
pub async fn models(app: &Arc<App>) -> Value {
    app.refresh_catalog(false).await;
    let catalog = app.catalog.lock().unwrap().clone();
    let accounts = app.store.lock().unwrap().accounts.clone();
    let mut data = vec![];
    for p in crate::store::PROVIDERS {
        let ms = catalog[p].as_array().cloned().unwrap_or_default();
        let owner = match p {
            "claude" => "anthropic",
            "codex" => "openai",
            _ => "google",
        };
        let entry = |mid: String, name: String| json!({"id":mid,"name":name,"object":"model","created":now() as u64,"owned_by":owner});
        if !app.config.require_account && config::flag("REMOTE_MODELS_AUTO", true) {
            for m in &ms {
                data.push(entry(
                    text(m, "id").into(),
                    format!("Auto · {}", text(m, "name")),
                ));
            }
        }
        if config::flag("REMOTE_MODELS_PER_ACCOUNT", true) {
            for a in accounts
                .iter()
                .filter(|a| text(a, "provider") == p && a["enabled"] == true)
            {
                for m in &ms {
                    data.push(entry(
                        format!("{}/{}", text(a, "slug"), text(m, "id")),
                        format!("{} · {}", text(a, "label"), text(m, "name")),
                    ));
                }
            }
        }
    }
    json!({"object":"list","data":data})
}
fn data(v: &Value) -> String {
    format!("data: {v}\n\n")
}
pub fn event_response<S>(stream: S) -> Response
where
    S: Stream<Item = String> + Send + 'static,
{
    let stream = async_stream::stream! {futures_util::pin_mut!(stream);let mut ping=tokio::time::interval(Duration::from_secs(15));ping.tick().await;loop{tokio::select!{next=stream.next()=>match next{Some(s)=>yield Ok::<_,Infallible>(s),None=>break},_=ping.tick()=>yield Ok(": ping\n\n".into())}}};
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap()
}
fn validated(body: &Value) -> Result<()> {
    if !body.is_object() {
        return Err(Error::new(400, "The body must be a JSON object"));
    }
    for key in [
        "model",
        "reasoning_effort",
        "instructions",
        "previous_response_id",
    ] {
        if let Some(v) = body.get(key) {
            if !v.is_null() && !v.is_string() {
                return Err(Error::new(400, format!("{key} must be a string")).param(key));
            }
        }
    }
    for key in ["stream", "store", "echo", "parallel_tool_calls"] {
        if let Some(v) = body.get(key) {
            if !v.is_boolean() {
                return Err(Error::new(400, format!("{key} must be a boolean")).param(key));
            }
        }
    }
    for key in [
        "response_format",
        "reasoning",
        "text",
        "stream_options",
        "metadata",
    ] {
        if let Some(v) = body.get(key) {
            if !v.is_null() && !v.is_object() {
                return Err(Error::new(400, format!("{key} must be an object")).param(key));
            }
        }
    }
    Ok(())
}
pub async fn handle(
    app: Arc<App>,
    method: Method,
    uri: Uri,
    h: HeaderMap,
    bytes: Bytes,
) -> Result<Response> {
    let ident = app.api_auth(&h)?;
    let path = uri.path().trim_start_matches("/v1/");
    if method == Method::GET && path == "models" {
        return Ok(Json(models(&app).await).into_response());
    }
    if method == Method::GET && path.starts_with("models/") {
        let name = path.trim_start_matches("models/");
        return Ok(Json(
            json!({"id":name,"object":"model","created":now() as u64,"owned_by":"customremote"}),
        )
        .into_response());
    }
    if method != Method::POST || !["chat/completions", "completions", "responses"].contains(&path) {
        return Err(
            Error::new(404, format!("Endpoint /v1/{path} is not supported")).code("unknown_url"),
        );
    }
    let body: Value = serde_json::from_slice(&bytes)?;
    validated(&body)?;
    let n = body
        .get("n")
        .map(|v| {
            v.as_u64()
                .filter(|n| *n >= 1 && *n <= 8)
                .ok_or_else(|| Error::new(400, "n must be an integer between 1 and 8").param("n"))
        })
        .transpose()?
        .unwrap_or(1) as usize;
    let (owner, exempt) = app.subject(&h, &ident);
    app.admit(
        &owner,
        exempt,
        if path == "chat/completions" { n } else { 1 },
    )?;
    let model = resolve(&app, text(&body, "model")).await?;
    match path {
        "chat/completions" => chat(app, model, body, n).await,
        "completions" => completion(app, model, body).await,
        _ => responses(app, model, body, owner).await,
    }
}
fn format_parts(body: &Value, responses: bool) -> Result<(Value, Option<Value>, bool)> {
    let fmt = if responses {
        body["text"]["format"].clone()
    } else {
        body["response_format"].clone()
    };
    prompt::format_prompt(&fmt)?;
    let js = fmt.get("json_schema").unwrap_or(&fmt);
    let schema = if fmt["type"] == "json_schema" && js["strict"] == true {
        Some(js["schema"].clone())
    } else {
        None
    };
    let json_mode = ["json_object", "json_schema"].contains(&text(&fmt, "type"));
    Ok((fmt, schema, json_mode))
}
fn request_prompt(
    c: &Conversation,
    body: &Value,
    tools: &[Value],
    choice: &Value,
    fmt: &Value,
    schema: Option<Value>,
    response: bool,
) -> Result<(Prompt, bool)> {
    let use_tools = !tools.is_empty() && choice != "none";
    let extras = [
        if use_tools {
            prompt::tools_prompt(tools, choice, body["parallel_tool_calls"] != false)
        } else {
            String::new()
        },
        prompt::format_prompt(fmt)?,
    ];
    let (system, blocks) = prompt::build(c, &extras);
    let effort = if response {
        text(&body["reasoning"], "effort")
    } else {
        text(body, "reasoning_effort")
    };
    Ok((
        Prompt {
            system,
            blocks,
            model: String::new(),
            effort: effort.into(),
            schema,
        },
        use_tools,
    ))
}
async fn collect(
    app: Arc<App>,
    model: Model,
    p: Prompt,
    stops: Vec<String>,
    tools: bool,
    json_mode: bool,
) -> Result<(String, Vec<Value>, Value)> {
    let mut stream = run(app, model, p);
    let mut filter = Filter::new(stops, tools);
    let mut output = String::new();
    let mut info = json!({});
    while let Some(ev) = stream.next().await {
        match ev? {
            Event::Text(t) => {
                output += &filter.feed(&t);
                if filter.stopped {
                    break;
                }
            }
            Event::Done(i) => info = i,
        }
    }
    output += &filter.flush();
    let (output, calls) = if tools {
        prompt::parse_calls(&filter.raw)
    } else {
        (output, vec![])
    };
    let output = if json_mode && calls.is_empty() {
        prompt::clean_json(&output)
    } else {
        output
    };
    Ok((output, calls, info))
}
async fn chat(app: Arc<App>, model: Model, body: Value, n: usize) -> Result<Response> {
    let c = prompt::chat(&body["messages"]).await?;
    let (tools, choice) = prompt::tools(&body)?;
    let (fmt, schema, json_mode) = format_parts(&body, false)?;
    let (p, use_tools) = request_prompt(&c, &body, &tools, &choice, &fmt, schema, false)?;
    let stops = prompt::stops(&body["stop"])?;
    let cid = id("chatcmpl-");
    let created = now() as u64;
    if body["stream"] == true {
        if n != 1 {
            return Err(Error::new(400, "n > 1 is not available when streaming").param("n"));
        }
        let include = body["stream_options"]["include_usage"] == true;
        let stream = async_stream::stream! {
                    let chunk=|delta:Value,finish:Value,usage:Value,choices:bool|{let mut v=json!({"id":cid,"object":"chat.completion.chunk","created":created,"model":model.public,"system_fingerprint":null,"choices":if choices{vec![json!({"index":0,"delta":delta,"logprobs":null,"finish_reason":finish})]}else{vec![]}});if include{v["usage"]=usage;}data(&v)};
                    yield chunk(json!({"role":"assistant","content":"","refusal":null}),Value::Null,Value::Null,true);
                    let mut filter=Filter::new(stops,use_tools);let mut info=json!({});let mut failed=false;let mut stream=run(app,model.clone(),p);
                    while let Some(ev)=stream.next().await{match ev{Ok(Event::Text(t))=>{let out=filter.feed(&t);if !out.is_empty(){yield chunk(json!({"content":out}),Value::Null,Value::Null,true);}
        if filter.stopped{break}},Ok(Event::Done(i))=>info=i,Err(e)=>{yield data(&e.body());failed=true;break}}}drop(stream);
                    if !failed {let out=filter.flush();if !out.is_empty(){yield chunk(json!({"content":out}),Value::Null,Value::Null,true);}let calls=if use_tools{prompt::parse_calls(&filter.raw).1}else{vec![]};for (i,c) in calls.iter().enumerate(){yield chunk(json!({"tool_calls":[{"index":i,"id":c["id"],"type":"function","function":{"name":c["name"],"arguments":""}}]}),Value::Null,Value::Null,true);yield chunk(json!({"tool_calls":[{"index":i,"function":{"arguments":c["arguments"]}}]}),Value::Null,Value::Null,true);}
                        yield chunk(json!({}),json!(if calls.is_empty(){"stop"}else{"tool_calls"}),Value::Null,true);if include{let (p,c,cached,reasoning)=prompt::usage(&info["usage"]);yield chunk(json!({}),Value::Null,json!({"prompt_tokens":p,"completion_tokens":c,"total_tokens":p+c,"prompt_tokens_details":{"cached_tokens":cached},"completion_tokens_details":{"reasoning_tokens":reasoning}}),false);}
                    }yield "data: [DONE]\n\n".into();
                };
        return Ok(event_response(stream));
    }
    let results=futures_util::future::try_join_all((0..n).map(|i|{let app=app.clone();let model=model.clone();let p=p.clone();let stops=stops.clone();async move{let (text,calls,info)=collect(app,model,p,stops,use_tools,json_mode).await?;let mut msg=json!({"role":"assistant","content":if text.is_empty()&&!calls.is_empty(){Value::Null}else{json!(text)},"refusal":null,"annotations":[]});if !calls.is_empty(){msg["tool_calls"]=json!(calls.iter().map(|c|json!({"id":c["id"],"type":"function","function":{"name":c["name"],"arguments":c["arguments"]}})).collect::<Vec<_>>());}Ok::<_,Error>((json!({"index":i,"message":msg,"logprobs":null,"finish_reason":if calls.is_empty(){"stop"}else{"tool_calls"}}),info))}})).await?;
    let (mut pt, mut ct, mut ca, mut rt) = (0, 0, 0, 0);
    for (_, i) in &results {
        let (p, c, cached, r) = prompt::usage(&i["usage"]);
        pt = pt.max(p);
        ct += c;
        ca = ca.max(cached);
        rt += r;
    }
    Ok(Json(json!({"id":cid,"object":"chat.completion","created":created,"model":results.first().and_then(|(_,i)|i.get("model")).cloned().unwrap_or(json!(model.public)),"system_fingerprint":null,"service_tier":"default","choices":results.iter().map(|(c,_)|c).collect::<Vec<_>>(),"usage":{"prompt_tokens":pt,"completion_tokens":ct,"total_tokens":pt+ct,"prompt_tokens_details":{"cached_tokens":ca,"audio_tokens":0},"completion_tokens_details":{"reasoning_tokens":rt,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0}}})).into_response())
}
async fn completion(app: Arc<App>, model: Model, body: Value) -> Result<Response> {
    let input = body.get("prompt").cloned().unwrap_or(json!(""));
    let prompt = if let Some(s) = input.as_str() {
        s.to_string()
    } else if let Some(a) = input.as_array() {
        a.first().and_then(Value::as_str).unwrap_or("").into()
    } else {
        return Err(Error::new(400, "prompt must be a string or list"));
    };
    let p=Prompt{system:"You are a raw text completion engine. Output only the direct continuation of the user's text, with no preamble, quotes or commentary.".into(),blocks:vec![json!({"type":"text","text":prompt})],model:String::new(),effort:String::new(),schema:None};
    let stops = prompt::stops(&body["stop"])?;
    let echo = if body["echo"] == true {
        prompt
    } else {
        String::new()
    };
    let cid = id("cmpl-");
    let created = now() as u64;
    if body["stream"] == true {
        return Ok(event_response(async_stream::stream! {
            let chunk = |t: String, finish: Value| {
                data(
                    &json!({"id":cid,"object":"text_completion","created":created,"model":model.public,"choices":[{"text":t,"index":0,"logprobs":null,"finish_reason":finish}]}),
                )
            };
            let mut f = Filter::new(stops, false);
            let mut failed = false;
            if !echo.is_empty() {
                yield chunk(echo,Value::Null);
            }
            let mut stream = run(app, model.clone(), p);
            while let Some(ev) = stream.next().await {
                match ev {
                    Ok(Event::Text(t)) => {
                        let out = f.feed(&t);
                        if !out.is_empty() {
                            yield chunk(out,Value::Null);
                        }
                        if f.stopped {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        failed = true;
                        yield data(&e.body());
                        break;
                    }
                }
            }
            drop(stream);
            if !failed {
                let out = f.flush();
                if !out.is_empty() {
                    yield chunk(out,Value::Null);
                }
                yield chunk(String::new(),json!("stop"));
            }
            yield "data: [DONE]\n\n".into();
        }));
    }
    let (output, _, info) = collect(app, model.clone(), p, stops, false, false).await?;
    let (pt, ct, _, _) = prompt::usage(&info["usage"]);
    Ok(Json(json!({"id":cid,"object":"text_completion","created":created,"model":info.get("model").cloned().unwrap_or(json!(model.public)),"choices":[{"text":echo+&output,"index":0,"logprobs":null,"finish_reason":"stop"}],"usage":{"prompt_tokens":pt,"completion_tokens":ct,"total_tokens":pt+ct}})).into_response())
}
fn response_final(
    app: &App,
    base: &Value,
    c: &Conversation,
    owner: &str,
    result: (String, Vec<Value>, Value),
    msg_id: &str,
) -> Value {
    let (text, calls, info) = result;
    let mut output = vec![];
    if !text.is_empty() || calls.is_empty() {
        output.push(json!({"type":"message","id":msg_id,"status":"completed","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[],"logprobs":[]}]}));
    }
    for call in &calls {
        output.push(json!({"type":"function_call","id":id("fc_"),"call_id":call["id"],"name":call["name"],"arguments":call["arguments"],"status":"completed"}));
    }
    let (pt, ct, cached, reasoning) = prompt::usage(&info["usage"]);
    let mut r = base.clone();
    r["status"] = json!("completed");
    r["output"] = json!(output);
    if let Some(m) = info.get("model") {
        r["model"] = m.clone();
    }
    r["usage"] = json!({"input_tokens":pt,"input_tokens_details":{"cached_tokens":cached},"output_tokens":ct,"output_tokens_details":{"reasoning_tokens":reasoning},"total_tokens":pt+ct});
    if r["store"] != false {
        let mut turns = c.turns.clone();
        turns.push(
            json!({"role":"assistant","blocks":[{"type":"text","text":text}],"tool_calls":calls}),
        );
        app.responses
            .lock()
            .unwrap()
            .remember(text_fn(base, "id"), owner.into(), turns);
    }
    r
}
fn text_fn(v: &Value, key: &str) -> String {
    text(v, key).to_owned()
}
async fn responses(app: Arc<App>, model: Model, body: Value, owner: String) -> Result<Response> {
    let mut c = prompt::responses(body.get("input").unwrap_or(&json!(""))).await?;
    if let Some(prev) = body.get("previous_response_id").and_then(Value::as_str) {
        let mut history = app
            .responses
            .lock()
            .unwrap()
            .get(prev, &owner)
            .ok_or_else(|| {
                Error::new(404, "Previous response not found").param("previous_response_id")
            })?;
        history.append(&mut c.turns);
        c.turns = history;
    }
    if let Some(s) = body.get("instructions").and_then(Value::as_str) {
        c.system.insert(0, s.into());
    }
    let (tools, choice) = prompt::tools(&body)?;
    let (fmt, schema, json_mode) = format_parts(&body, true)?;
    let (p, use_tools) = request_prompt(&c, &body, &tools, &choice, &fmt, schema, true)?;
    let rid = id("resp_");
    let msg_id = id("msg_");
    let base = json!({"id":rid,"object":"response","created_at":now() as u64,"status":"in_progress","model":model.public,"output":[],"error":null,"incomplete_details":null,"instructions":body["instructions"],"max_output_tokens":body["max_output_tokens"],"parallel_tool_calls":body.get("parallel_tool_calls").unwrap_or(&json!(true)),"previous_response_id":body["previous_response_id"],"reasoning":body.get("reasoning").unwrap_or(&json!({"effort":null,"summary":null})),"store":body.get("store").unwrap_or(&json!(true)),"temperature":body.get("temperature").unwrap_or(&json!(1.0)),"text":body.get("text").unwrap_or(&json!({"format":{"type":"text"}})),"tool_choice":choice,"tools":tools,"top_p":body.get("top_p").unwrap_or(&json!(1.0)),"truncation":"disabled","usage":null,"user":body["user"],"metadata":body.get("metadata").unwrap_or(&json!({}))});
    if body["stream"] != true {
        let (text, calls, info) =
            collect(app.clone(), model, p, vec![], use_tools, json_mode).await?;
        return Ok(Json(response_final(
            &app,
            &base,
            &c,
            &owner,
            (text, calls, info),
            &msg_id,
        ))
        .into_response());
    }
    Ok(event_response(async_stream::stream! {
        let mut seq = 0;
        let mut ev = |name: &str, mut v: Value| {
            seq += 1;
            v["type"] = json!(name);
            v["sequence_number"] = json!(seq);
            format!("event: {name}\ndata: {v}\n\n")
        };
        yield ev("response.created",json!({"response":base}));
        yield ev("response.in_progress",json!({"response":base}));
        let mut f = Filter::new(vec![], use_tools);
        let mut info = json!({});
        let mut output = String::new();
        let mut started = false;
        let mut failed = false;
        let mut stream = run(app.clone(), model, p);
        while let Some(item) = stream.next().await {
            match item {
                Ok(Event::Done(i)) => info = i,
                Err(e) => {
                    let mut failed_base = base.clone();
                    failed_base["status"] = json!("failed");
                    failed_base["error"] =
                        json!({"code":e.code.unwrap_or(e.kind),"message":e.message});
                    yield ev("response.failed",json!({"response":failed_base}));
                    failed = true;
                    break;
                }
                Ok(Event::Text(t)) => {
                    let out = f.feed(&t);
                    if !out.is_empty() {
                        if !started {
                            started = true;
                            yield ev("response.output_item.added",json!({"output_index":0,"item":{"type":"message","id":msg_id,"status":"in_progress","role":"assistant","content":[]}}));
                            yield ev("response.content_part.added",json!({"item_id":msg_id,"output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[],"logprobs":[]}}));
                        }
                        output += &out;
                        yield ev("response.output_text.delta",json!({"item_id":msg_id,"output_index":0,"content_index":0,"delta":out,"logprobs":[]}));
                    }
                }
            }
        }
        if !failed {
            let rest = f.flush();
            let calls = if use_tools {
                prompt::parse_calls(&f.raw).1
            } else {
                vec![]
            };
            if !started && (!rest.is_empty() || calls.is_empty()) {
                started = true;
                yield ev("response.output_item.added",json!({"output_index":0,"item":{"type":"message","id":msg_id,"status":"in_progress","role":"assistant","content":[]}}));
                yield ev("response.content_part.added",json!({"item_id":msg_id,"output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[],"logprobs":[]}}));
            }
            if !rest.is_empty() {
                output += &rest;
                yield ev("response.output_text.delta",json!({"item_id":msg_id,"output_index":0,"content_index":0,"delta":rest,"logprobs":[]}));
            }
            let final_ = response_final(
                &app,
                &base,
                &c,
                &owner,
                (output.clone(), calls, info),
                &msg_id,
            );
            let mut idx = 0;
            if started {
                let part =
                    json!({"type":"output_text","text":output,"annotations":[],"logprobs":[]});
                yield ev("response.output_text.done",json!({"item_id":msg_id,"output_index":0,"content_index":0,"text":output,"logprobs":[]}));
                yield ev("response.content_part.done",json!({"item_id":msg_id,"output_index":0,"content_index":0,"part":part}));
                yield ev("response.output_item.done",json!({"output_index":0,"item":{"type":"message","id":msg_id,"status":"completed","role":"assistant","content":[part]}}));
                idx = 1;
            }
            for item in final_["output"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["type"] == "function_call")
            {
                let mut pending = item.clone();
                pending["arguments"] = json!("");
                pending["status"] = json!("in_progress");
                yield ev("response.output_item.added",json!({"output_index":idx,"item":pending}));
                yield ev("response.function_call_arguments.delta",json!({"item_id":item["id"],"output_index":idx,"delta":item["arguments"]}));
                yield ev("response.function_call_arguments.done",json!({"item_id":item["id"],"output_index":idx,"arguments":item["arguments"]}));
                yield ev("response.output_item.done",json!({"output_index":idx,"item":item}));
                idx += 1;
            }
            yield ev("response.completed",json!({"response":final_}));
        }
    }))
}
