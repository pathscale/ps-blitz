use super::*;
use regex::{Captures, Regex, RegexBuilder};

const MAX_PATTERN: usize = 65_536;
const MAX_MATCHES: usize = 65_536;

fn compiled(value: &Value) -> Result<Rc<Regex>> {
    value
        .as_object()
        .and_then(|object| {
            object
                .borrow()
                .payload
                .as_ref()
                .and_then(|payload| Rc::clone(payload).downcast::<Regex>().ok())
        })
        .ok_or_else(|| Error::type_error("receiver is not a compiled regular expression"))
}
fn utf16_offset(text: &str, byte: usize) -> usize {
    text[..byte].encode_utf16().count()
}
fn byte_offset(text: &str, utf16: usize) -> Option<usize> {
    let mut count = 0;
    for (offset, ch) in text.char_indices() {
        if count == utf16 {
            return Some(offset);
        }
        count += ch.len_utf16();
    }
    (count == utf16).then_some(text.len())
}
fn replacement_text(template: &str, captures: &Captures<'_>, input: &str) -> String {
    let Some(whole) = captures.get(0) else {
        return String::new();
    };
    let mut result = String::new();
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            result.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('$') => {
                chars.next();
                result.push('$');
            }
            Some('&') => {
                chars.next();
                result.push_str(whole.as_str());
            }
            Some('`') => {
                chars.next();
                result.push_str(&input[..whole.start()]);
            }
            Some('\'') => {
                chars.next();
                result.push_str(&input[whole.end()..]);
            }
            Some(digit) if digit.is_ascii_digit() && digit != '0' => {
                chars.next();
                let first = digit.to_digit(10).unwrap_or(0) as usize;
                let mut index = first;
                if let Some(next) = chars.peek().and_then(|ch| ch.to_digit(10)) {
                    let candidate = index * 10 + next as usize;
                    if candidate < captures.len() {
                        index = candidate;
                        chars.next();
                    }
                }
                if index < captures.len() {
                    if let Some(value) = captures.get(index) {
                        result.push_str(value.as_str());
                    }
                } else {
                    result.push('$');
                    result.push(digit);
                }
            }
            _ => result.push('$'),
        }
    }
    result
}
impl<B: RuntimeBridge> Guest<B> {
    pub(super) fn regexp_get(&mut self, value: &Value, key: &str) -> Option<Value> {
        if value.kind() == "RegExp" && matches!(key, "exec" | "test" | "toString") {
            return Some(Value::native(format!("RegExp.{key}")));
        }
        if matches!(value, Value::String(_))
            && matches!(key, "replace" | "replaceAll" | "match" | "search" | "split")
        {
            return Some(Value::native(format!("RegexString.{key}")));
        }
        None
    }
    pub(super) fn regexp_call(
        &mut self,
        doc: &mut BaseDocument,
        name: &str,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Option<Value>> {
        let argument = args.first().cloned().unwrap_or(Value::Undefined);
        match name {
            "RegExp" => {
                let source = if argument.kind() == "RegExp" {
                    argument.get("source").text()
                } else if matches!(argument, Value::Undefined) {
                    String::new()
                } else {
                    argument.text()
                };
                let flags = args
                    .get(1)
                    .filter(|value| !matches!(value, Value::Undefined))
                    .map_or_else(
                        || {
                            if argument.kind() == "RegExp" {
                                argument.get("flags").text()
                            } else {
                                String::new()
                            }
                        },
                        Value::text,
                    );
                if source.len() > MAX_PATTERN {
                    return Err(Error::new(
                        "RangeError",
                        "regular expression source limit exceeded",
                    ));
                }
                let mut seen = String::new();
                for flag in flags.chars() {
                    if !"gimsuy".contains(flag) || seen.contains(flag) {
                        return Err(Error::syntax(
                            "unsupported or duplicate regular expression flag",
                        ));
                    }
                    seen.push(flag);
                }
                let regex = RegexBuilder::new(&source)
                    .case_insensitive(flags.contains('i'))
                    .multi_line(flags.contains('m'))
                    .dot_matches_new_line(flags.contains('s'))
                    .size_limit(4 * 1024 * 1024)
                    .dfa_size_limit(4 * 1024 * 1024)
                    .nest_limit(128)
                    .build()
                    .map_err(|error| Error::syntax(error.to_string()))?;
                let value = Value::object("RegExp");
                if let Some(object) = value.as_object() {
                    object.borrow_mut().payload = Some(Rc::new(regex));
                }
                value.put("source", Value::string(source));
                value.put("flags", Value::string(&flags));
                value.put("global", Value::Bool(flags.contains('g')));
                value.put("sticky", Value::Bool(flags.contains('y')));
                value.put("lastIndex", Value::Number(0.0));
                Ok(Some(value))
            }
            "RegExp.toString" => Ok(Some(Value::string(format!(
                "/{}/{}",
                this.get("source").text(),
                this.get("flags").text()
            )))),
            "RegExp.exec" | "RegExp.test" => {
                let input = argument.text();
                let regex = compiled(&this)?;
                let indexed = this.get("global").truthy() || this.get("sticky").truthy();
                let start = if indexed {
                    byte_offset(
                        &input,
                        this.get("lastIndex").number_value().max(0.0) as usize,
                    )
                } else {
                    Some(0)
                };
                let captures = start
                    .and_then(|start| regex.captures_at(&input, start))
                    .filter(|captures| {
                        !this.get("sticky").truthy()
                            || captures
                                .get(0)
                                .is_some_and(|matched| Some(matched.start()) == start)
                    });
                let Some(captures) = captures else {
                    if indexed {
                        this.put("lastIndex", Value::Number(0.0));
                    }
                    return Ok(Some(if name.ends_with("test") {
                        Value::Bool(false)
                    } else {
                        Value::Null
                    }));
                };
                if indexed {
                    if let Some(whole) = captures.get(0) {
                        this.put(
                            "lastIndex",
                            Value::Number(utf16_offset(&input, whole.end()) as f64),
                        );
                    }
                }
                if name.ends_with("test") {
                    return Ok(Some(Value::Bool(true)));
                }
                let value = Value::array(
                    captures
                        .iter()
                        .map(|capture| {
                            capture
                                .map_or(Value::Undefined, |capture| Value::string(capture.as_str()))
                        })
                        .collect(),
                );
                value.put(
                    "index",
                    Value::Number(
                        captures
                            .get(0)
                            .map_or(0, |matched| utf16_offset(&input, matched.start()))
                            as f64,
                    ),
                );
                value.put("input", Value::string(input));
                Ok(Some(value))
            }
            _ if name.starts_with("RegexString.") => {
                let input = this.text();
                let operation = name.trim_start_matches("RegexString.");
                let replacement = args.get(1).cloned().unwrap_or(Value::Undefined);
                if argument.kind() != "RegExp" && matches!(operation, "replace" | "replaceAll") {
                    let needle = argument.text();
                    let global = operation == "replaceAll";
                    let mut result = String::new();
                    let mut last = 0;
                    for (start, matched) in
                        input
                            .match_indices(&needle)
                            .take(if global { MAX_MATCHES } else { 1 })
                    {
                        self.tick()?;
                        result.push_str(&input[last..start]);
                        if matches!(replacement.kind().as_str(), "Native" | "Function") {
                            let value = self.call(
                                doc,
                                replacement.clone(),
                                Value::Undefined,
                                vec![
                                    Value::string(matched),
                                    Value::Number(utf16_offset(&input, start) as f64),
                                    Value::string(&input),
                                ],
                                false,
                            )?;
                            result.push_str(&value.text());
                        } else {
                            result.push_str(
                                &replacement.text().replace("$$", "$").replace("$&", matched),
                            );
                        }
                        last = start + matched.len();
                    }
                    result.push_str(&input[last..]);
                    return Ok(Some(Value::string(result)));
                }
                if argument.kind() != "RegExp" && operation == "split" {
                    return self.builtin_call(doc, "String.prototype.split", this, args, false);
                }
                let pattern = if argument.kind() == "RegExp" {
                    argument.clone()
                } else {
                    self.regexp_call(doc, "RegExp", Value::Undefined, vec![argument.clone()])?
                        .unwrap_or(Value::Undefined)
                };
                let regex = compiled(&pattern)?;
                let global = pattern.get("global").truthy();
                if operation == "search" {
                    return Ok(Some(Value::Number(
                        regex
                            .find(&input)
                            .map_or(-1.0, |matched| utf16_offset(&input, matched.start()) as f64),
                    )));
                }
                if operation == "match" && !global {
                    return self.regexp_call(
                        doc,
                        "RegExp.exec",
                        pattern,
                        vec![Value::string(input)],
                    );
                }
                if operation == "match" {
                    let values = regex
                        .find_iter(&input)
                        .take(MAX_MATCHES)
                        .map(|matched| Value::string(matched.as_str()))
                        .collect::<Vec<_>>();
                    return Ok(Some(if values.is_empty() {
                        Value::Null
                    } else {
                        Value::array(values)
                    }));
                }
                if operation == "split" {
                    let limit = args
                        .get(1)
                        .filter(|value| !matches!(value, Value::Undefined))
                        .map_or(MAX_MATCHES, |value| value.number_value().max(0.0) as usize)
                        .min(MAX_MATCHES);
                    let mut result = Vec::new();
                    let mut end = 0;
                    for captures in regex.captures_iter(&input).take(MAX_MATCHES) {
                        let Some(matched) = captures.get(0) else {
                            continue;
                        };
                        if matched.start() == matched.end() && matched.start() == end {
                            continue;
                        }
                        result.push(Value::string(&input[end..matched.start()]));
                        for capture in captures.iter().skip(1) {
                            result.push(capture.map_or(Value::Undefined, |capture| {
                                Value::string(capture.as_str())
                            }));
                        }
                        end = matched.end();
                        if result.len() >= limit {
                            break;
                        }
                    }
                    result.push(Value::string(&input[end..]));
                    result.truncate(limit);
                    return Ok(Some(Value::array(result)));
                }
                if matches!(operation, "replace" | "replaceAll") {
                    if operation == "replaceAll" && !global {
                        return Err(Error::type_error(
                            "replaceAll requires a global regular expression",
                        ));
                    }
                    let mut result = String::new();
                    let mut end = 0;
                    for captures in
                        regex
                            .captures_iter(&input)
                            .take(if global { MAX_MATCHES } else { 1 })
                    {
                        self.tick()?;
                        let Some(matched) = captures.get(0) else {
                            continue;
                        };
                        result.push_str(&input[end..matched.start()]);
                        if matches!(replacement.kind().as_str(), "Native" | "Function") {
                            let mut args = captures
                                .iter()
                                .map(|capture| {
                                    capture.map_or(Value::Undefined, |capture| {
                                        Value::string(capture.as_str())
                                    })
                                })
                                .collect::<Vec<_>>();
                            args.push(Value::Number(utf16_offset(&input, matched.start()) as f64));
                            args.push(Value::string(&input));
                            result.push_str(
                                &self
                                    .call(doc, replacement.clone(), Value::Undefined, args, false)?
                                    .text(),
                            );
                        } else {
                            result.push_str(&replacement_text(
                                &replacement.text(),
                                &captures,
                                &input,
                            ));
                        }
                        end = matched.end();
                    }
                    result.push_str(&input[end..]);
                    return Ok(Some(Value::string(result)));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}
