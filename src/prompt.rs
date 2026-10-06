use crate::{
    config::{id, text},
    content,
    error::{Error, Result},
};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;
#[derive(Clone)]
pub struct Conversation {
    pub system: Vec<String>,
    pub turns: Vec<Value>,
}
pub async fn chat(messages: &Value) -> Result<Conversation> {
    let messages = messages
        .as_array()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| Error::new(400, "messages must be a nonempty list").param("messages"))?;
    let mut c = Conversation {
        system: vec![],
        turns: vec![],
    };
    for m in messages {
        if !m.is_object() {
            return Err(Error::new(400, "Each message must be an object"));
        }
        let role = text(m, "role");
        let blocks = content::blocks(&m["content"]).await?;
        match role{
        "system"|"developer"=>c.system.push(content::text_of(&blocks)),
        "user"=>c.turns.push(json!({"role":"user","blocks":blocks})),
        "assistant"=>{let mut calls=vec![];if let Some(ts)=m.get("tool_calls"){for t in ts.as_array().ok_or_else(||Error::new(400,"tool_calls must be a list"))?{if !t["function"].is_object(){return Err(Error::new(400,"Invalid function call"))}calls.push(t["function"].clone());}}
if let Some(f)=m.get("function_call"){calls.push(f.clone());}c.turns.push(json!({"role":"assistant","blocks":blocks,"tool_calls":calls}));},
        "tool"|"function"=>c.turns.push(json!({"role":"tool","blocks":blocks,"tool_call_id":m.get("tool_call_id").unwrap_or(&json!("")),"name":m.get("name").unwrap_or(&json!(""))})),
        _=>return Err(Error::new(400,format!("Invalid role: {role}")).param("messages"))
    }
    }
    if c.turns.is_empty() {
        c.turns
            .push(json!({"role":"user","blocks":[{"type":"text","text":"Hello."}]}))
    }
    Ok(c)
}
pub async fn responses(input: &Value) -> Result<Conversation> {
    if let Some(s) = input.as_str() {
        return Ok(Conversation {
            system: vec![],
            turns: vec![json!({"role":"user","blocks":[{"type":"text","text":s}]} )],
        });
    }
    let items = input
        .as_array()
        .ok_or_else(|| Error::new(400, "input must be a string or list"))?;
    let mut c = Conversation {
        system: vec![],
        turns: vec![],
    };
    for i in items {
        if !i.is_object() {
            return Err(Error::new(400, "Each input item must be an object"));
        }
        match i.get("type").and_then(Value::as_str).unwrap_or("message") {
            "message" => {
                let role = i.get("role").and_then(Value::as_str).unwrap_or("user");
                let blocks = content::blocks(&i["content"]).await?;
                if ["system", "developer"].contains(&role) {
                    c.system.push(content::text_of(&blocks));
                } else {
                    c.turns.push(json!({"role":if role=="assistant"{"assistant"}else{"user"},"blocks":blocks,"tool_calls":[]}));
                }
            }
            "function_call" => {
                let call = json!({"name":i["name"],"arguments":i.get("arguments").unwrap_or(&json!("{}")),"id":i["call_id"]});
                if c.turns.last().is_some_and(|t| t["role"] == "assistant") {
                    c.turns.last_mut().unwrap()["tool_calls"]
                        .as_array_mut()
                        .unwrap()
                        .push(call);
                } else {
                    c.turns
                        .push(json!({"role":"assistant","blocks":[],"tool_calls":[call]}));
                }
            }
            "function_call_output" => {
                let output = &i["output"];
                let blocks = if output.is_array() {
                    content::blocks(output).await?
                } else {
                    vec![
                        json!({"type":"text","text":output.as_str().map(String::from).unwrap_or_else(||output.to_string())}),
                    ]
                };
                c.turns.push(
                    json!({"role":"tool","tool_call_id":i["call_id"],"name":"","blocks":blocks}),
                );
            }
            _ => {}
        }
    }
    Ok(c)
}
pub fn tools(body: &Value) -> Result<(Vec<Value>, Value)> {
    let mut tools = match body.get("tools") {
        Some(v) => v
            .as_array()
            .cloned()
            .ok_or_else(|| Error::new(400, "tools must be a list"))?,
        None => vec![],
    };
    let mut choice = body.get("tool_choice").cloned().unwrap_or(json!("auto"));
    if tools.is_empty() {
        if let Some(f) = body.get("functions") {
            tools = f
                .as_array()
                .ok_or_else(|| Error::new(400, "functions must be a list"))?
                .iter()
                .map(|v| json!({"type":"function","function":v}))
                .collect();
            choice = body.get("function_call").cloned().unwrap_or(json!("auto"));
        }
    }
    for t in &tools {
        if !t.is_object() {
            return Err(Error::new(400, "Invalid tool"));
        }
    }
    Ok((tools, choice))
}
pub fn tools_prompt(tools: &[Value], choice: &Value, parallel: bool) -> String {
    let functions:Vec<Value>=tools.iter().filter_map(|t|{let f=t.get("function").unwrap_or(t);if text(f,"name").is_empty(){None}else{Some(json!({"name":f["name"],"description":f.get("description").unwrap_or(&json!("")),"parameters":f.get("parameters").unwrap_or(&json!({"type":"object","properties":{}}))}))}}).collect();
    if functions.is_empty() {
        return String::new();
    }
    let mut s=format!("# Function calling\nYou can call the following functions, described with JSON Schema:\n<functions>\n{}\n</functions>\nTo call a function, output exactly:\n<tool_call>\n{{\"name\": \"function name\", \"arguments\": {{}}}}\n</tool_call>\nOnly call the listed functions with matching arguments. You may write a short sentence before calls, but nothing after the last </tool_call>. Stop after calling: results arrive inside <tool_result> blocks. If no function is needed, answer normally.\n",functions.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"));
    s += if parallel {
        "Several blocks may be emitted for parallel calls.\n"
    } else {
        "Call at most one function per message.\n"
    };
    if choice == "required" {
        s += "You MUST call at least one function.\n"
    } else if choice.is_object() {
        let name = if choice["function"].is_object() {
            text(&choice["function"], "name")
        } else {
            text(choice, "name")
        };
        s += &format!("You MUST call the function `{name}`.\n");
    }
    s
}
pub fn format_prompt(fmt: &Value) -> Result<String> {
    if fmt.is_null() {
        return Ok(String::new());
    }
    if !fmt.is_object() {
        return Err(Error::new(400, "response_format must be an object"));
    }
    Ok(match text(fmt,"type"){"json_object"=>"# Output format\nRespond with a single valid JSON object only: no prose, no code fences.".into(),"json_schema"=>format!("# Output format\nRespond with a single valid JSON value only, without prose or code fences, conforming to this JSON Schema:\n{}",fmt.get("json_schema").unwrap_or(fmt)["schema"]),_=>String::new()})
}
pub fn render_call(call: &Value) -> String {
    let arg = call.get("arguments").cloned().unwrap_or(json!({}));
    let arg = if let Some(s) = arg.as_str() {
        serde_json::from_str(s).unwrap_or(arg)
    } else {
        arg
    };
    format!(
        "<tool_call>\n{}\n</tool_call>",
        json!({"name":call["name"],"arguments":arg})
    )
}
fn add_text(blocks: &mut Vec<Value>, s: &str) {
    if let Some(last) = blocks.last_mut().filter(|b| b["type"] == "text") {
        last["text"] = json!(format!("{}{s}", text(last, "text")))
    } else {
        blocks.push(json!({"type":"text","text":s}))
    }
}
pub fn build(c: &Conversation, extra: &[String]) -> (String, Vec<Value>) {
    let mut system = c
        .system
        .iter()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    if system.is_empty() {
        system = "You are a helpful assistant.".into();
    }
    if c.turns.len() == 1 && c.turns[0]["role"] == "user" {
        let mut blocks = c.turns[0]["blocks"].as_array().cloned().unwrap_or_default();
        if blocks.is_empty() {
            blocks.push(json!({"type":"text","text":"(empty)"}));
        }
        for e in extra.iter().filter(|s| !s.is_empty()) {
            system += &format!("\n\n{e}");
        }
        return (system, blocks);
    }
    system+="\n\n# Conversation format\nThe user message contains the conversation so far inside <conversation> tags. Write only the next assistant message, as yourself, without any role tag.";
    let mut blocks = vec![];
    add_text(&mut blocks, "<conversation>\n");
    for t in &c.turns {
        let parts = t["blocks"].as_array().cloned().unwrap_or_default();
        match text(t, "role") {
            "user" => {
                add_text(&mut blocks, "<user>\n");
                for b in parts {
                    if b["type"] == "text" {
                        add_text(&mut blocks, text(&b, "text"))
                    } else {
                        blocks.push(b)
                    }
                }
                add_text(&mut blocks, "\n</user>\n");
            }
            "assistant" => {
                let calls = t["tool_calls"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(render_call)
                    .collect::<Vec<_>>()
                    .join("\n");
                add_text(
                    &mut blocks,
                    &format!(
                        "<assistant>\n{}\n{calls}\n</assistant>\n",
                        content::text_of(&parts)
                    ),
                );
            }
            "tool" => add_text(
                &mut blocks,
                &format!(
                    "<tool_result tool_call_id=\"{}\" name=\"{}\">\n{}\n</tool_result>\n",
                    text(t, "tool_call_id"),
                    text(t, "name"),
                    content::text_of(&parts)
                ),
            ),
            _ => {}
        }
    }
    add_text(&mut blocks, "</conversation>");
    for e in extra.iter().filter(|s| !s.is_empty()) {
        system += &format!("\n\n{e}");
    }
    (system, blocks)
}
pub struct Filter {
    pub raw: String,
    buffer: String,
    markers: Vec<String>,
    closed: bool,
    pub stopped: bool,
}
impl Filter {
    pub fn new(stops: Vec<String>, tools: bool) -> Self {
        let mut markers = stops
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if tools {
            markers.extend(["<tool_call>".into(), "<function_calls>".into()]);
        }
        Self {
            raw: String::new(),
            buffer: String::new(),
            markers,
            closed: false,
            stopped: false,
        }
    }
    pub fn feed(&mut self, s: &str) -> String {
        if self.stopped {
            return String::new();
        }
        self.raw += s;
        if self.closed {
            return String::new();
        }
        self.buffer += s;
        if let Some((idx, m)) = self
            .markers
            .iter()
            .filter_map(|m| self.buffer.find(m).map(|i| (i, m)))
            .min_by_key(|(i, _)| *i)
        {
            self.closed = true;
            self.stopped = !["<tool_call>", "<function_calls>"].contains(&m.as_str());
            if self.stopped {
                self.raw.truncate(self.raw.len() - self.buffer.len() + idx);
            }
            let out = self.buffer[..idx].into();
            self.buffer.clear();
            return out;
        }
        let mut keep = 0;
        for m in &self.markers {
            for k in (1..m.len().min(self.buffer.len() + 1)).rev() {
                if m.is_char_boundary(k) && self.buffer.ends_with(&m[..k]) {
                    keep = keep.max(k);
                    break;
                }
            }
        }
        let len = self.buffer.len() - keep;
        self.buffer.drain(..len).collect()
    }
    pub fn flush(&mut self) -> String {
        if self.closed {
            self.buffer.clear();
            String::new()
        } else {
            std::mem::take(&mut self.buffer)
        }
    }
}
static TOOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<tool_call>\s*(.*?)\s*</tool_call>").unwrap());
static FCALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<function_calls>\s*(.*?)\s*</function_calls>").unwrap());
static INVOKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<invoke name="([^"]+)">(.*?)</invoke>"#).unwrap());
static PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<parameter name="([^"]+)">(.*?)</parameter>"#).unwrap());
pub fn clean_json(s: &str) -> String {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")) {
        if let Some(inner) = inner.strip_suffix("```") {
            return inner.trim().into();
        }
    }
    s.into()
}
fn json_calls(s: &str) -> Vec<Value> {
    let s = clean_json(s);
    let s = s.trim();
    if let Ok(Value::Array(a)) = serde_json::from_str(s) {
        return a;
    }
    serde_json::Deserializer::from_str(s)
        .into_iter::<Value>()
        .take_while(|v| v.is_ok())
        .filter_map(|v| v.ok())
        .flat_map(|v| v.as_array().cloned().unwrap_or(vec![v]))
        .collect()
}
pub fn parse_calls(raw: &str) -> (String, Vec<Value>) {
    let mut found = vec![];
    for c in TOOL.captures_iter(raw) {
        found.extend(json_calls(&c[1]));
    }
    for c in FCALL.captures_iter(raw) {
        if INVOKE.is_match(&c[1]) {
            for i in INVOKE.captures_iter(&c[1]) {
                let mut args = json!({});
                for p in PARAM.captures_iter(&i[2]) {
                    args[&p[1]] = serde_json::from_str(p[2].trim()).unwrap_or(json!(p[2].trim()));
                }
                found.push(json!({"name":&i[1],"arguments":args}));
            }
        } else {
            found.extend(json_calls(&c[1]));
        }
    }
    let calls:Vec<Value>=found.into_iter().filter(|v|!text(v,"name").is_empty()).map(|v|{let arg=v.get("arguments").or(v.get("parameters")).cloned().unwrap_or(json!({}));json!({"id":id("call_"),"name":v["name"],"arguments":arg.as_str().map(String::from).unwrap_or_else(||arg.to_string())})}).collect();
    if calls.is_empty() {
        return (raw.into(), calls);
    }
    let cut = ["<tool_call>", "<function_calls>"]
        .iter()
        .filter_map(|m| raw.find(m))
        .min()
        .unwrap_or(raw.len());
    (raw[..cut].trim().into(), calls)
}
pub fn stops(v: &Value) -> Result<Vec<String>> {
    if v.is_null() {
        return Ok(vec![]);
    }
    if let Some(s) = v.as_str() {
        return Ok(vec![s.into()]);
    }
    let a = v
        .as_array()
        .ok_or_else(|| Error::new(400, "stop must be a string or list"))?;
    if a.len() > 4 || a.iter().any(|v| !v.is_string()) {
        return Err(Error::new(400, "stop accepts at most four strings"));
    }
    Ok(a.iter()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect())
}
pub fn usage(u: &Value) -> (u64, u64, u64, u64) {
    let cached = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
    (
        u["input_tokens"].as_u64().unwrap_or(0)
            + cached
            + u["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        u["output_tokens"].as_u64().unwrap_or(0),
        cached,
        u["output_tokens_details"]["thinking_tokens"]
            .as_u64()
            .unwrap_or(0),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_stop_and_unicode() {
        let mut f = Filter::new(vec!["🛑stop".into()], false);
        assert_eq!(f.feed("Hello 🛑st"), "Hello ");
        assert_eq!(f.feed("op secret"), "");
        assert!(f.stopped);
        assert_eq!(f.raw, "Hello ");
    }
    #[test]
    fn hides_tool_markers_across_chunks() {
        let mut f = Filter::new(vec![], true);
        assert_eq!(f.feed("Okay <tool_"), "Okay ");
        assert_eq!(
            f.feed("call>{\"name\":\"test\",\"arguments\":{}}</tool_call>"),
            ""
        );
        let (text, calls) = parse_calls(&f.raw);
        assert_eq!(text, "Okay");
        assert_eq!(calls[0]["name"], "test");
    }
    #[test]
    fn parses_xml_calls() {
        let (_,c)=parse_calls("<function_calls><invoke name=\"weather\"><parameter name=\"city\">Paris</parameter></invoke></function_calls>");
        assert_eq!(c[0]["arguments"], "{\"city\":\"Paris\"}");
    }
}
