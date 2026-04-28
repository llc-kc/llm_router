use llm_tokenizer::{
    chat_template::{ChatTemplateContentFormat, ChatTemplateParams},
    huggingface::HuggingFaceTokenizer,
    traits::Tokenizer,
};
use openai_protocol::chat::{ChatCompletionRequest, ChatMessage};
use serde_json::Value;
use std::collections::HashMap;

/// Check if the request is a streaming request
pub fn is_streaming_request(request_value: &Value) -> bool {
    request_value.get("stream").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Process chat messages using chat template (ported from sgl-model-gateway)
pub fn process_chat_messages(request: &ChatCompletionRequest, tokenizer: &dyn Tokenizer) -> Result<String, String> {
    // Try to downcast to HuggingFaceTokenizer
    let hf_tokenizer = tokenizer
        .as_any()
        .downcast_ref::<HuggingFaceTokenizer>()
        .ok_or("Tokenizer must be HuggingFaceTokenizer with chat template support")?;

    // Get content format and transform messages accordingly
    let content_format = hf_tokenizer.chat_template_content_format();
    let mut transformed_messages = process_content_format(&request.messages, content_format)?;

    // Process tool call arguments in assistant messages
    process_tool_call_arguments(&mut transformed_messages)?;

    // Convert tools to JSON values for template processing
    let tools_json: Option<Vec<Value>> = request
        .tools
        .as_ref()
        .map(|tools| tools.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>())
        .transpose()
        .map_err(|e| format!("Failed to serialize tools: {}", e))?;

    let kwargs_capacity = 1 + request.chat_template_kwargs.as_ref().map_or(0, |k| k.len());
    let mut combined_template_kwargs = HashMap::with_capacity(kwargs_capacity);

    // Add reasoning_effort if present
    if let Some(reasoning_effort) = &request.reasoning_effort {
        combined_template_kwargs.insert("reasoning_effort".to_string(), Value::String(reasoning_effort.clone()));
    }

    // Add any additional template kwargs from request
    if let Some(template_kwargs) = &request.chat_template_kwargs {
        for (key, value) in template_kwargs {
            combined_template_kwargs.insert(key.clone(), value.clone());
        }
    }

    let final_template_kwargs_ref = if combined_template_kwargs.is_empty() {
        None
    } else {
        Some(&combined_template_kwargs)
    };

    // Handle assistant prefix for continue_final_message
    let assistant_prefix = if request.continue_final_message
        && !transformed_messages.is_empty()
        && transformed_messages
            .last()
            .and_then(|msg| msg.get("role"))
            .and_then(|v| v.as_str())
            == Some("assistant")
    {
        let last_msg = transformed_messages.pop().unwrap();
        last_msg.get("content").and_then(|v| v.as_str()).map(|s| s.to_string())
    } else {
        None
    };

    // Try to apply chat template with detected format first
    let params = ChatTemplateParams {
        add_generation_prompt: true,
        tools: tools_json.as_deref(),
        template_kwargs: final_template_kwargs_ref,
        ..Default::default()
    };

    log::debug!("transformed_messages: {:?}", transformed_messages);

    match hf_tokenizer.apply_chat_template(&transformed_messages, params) {
        Ok(rendered) => {
            log::debug!("rendered: {:?}", rendered);
            // Success with detected format
            if let Some(prefix) = assistant_prefix {
                Ok(format!("{}{}", rendered, prefix))
            } else {
                Ok(rendered)
            }
        },
        Err(e) => {
            // If detected format was OpenAI, try falling back to String format
            // This handles cases where the format detection incorrectly returns OpenAI
            // but the template actually expects string content (e.g., DeepSeek)
            if content_format == ChatTemplateContentFormat::OpenAI {
                let string_format_messages = process_content_format_string(&request.messages)?;
                // Re-process tool call arguments for string format messages
                process_tool_call_arguments(&mut string_format_messages.clone())?;
                let fallback_params = ChatTemplateParams {
                    add_generation_prompt: true,
                    tools: tools_json.as_deref(),
                    template_kwargs: final_template_kwargs_ref,
                    ..Default::default()
                };
                match hf_tokenizer.apply_chat_template(&string_format_messages, fallback_params) {
                    Ok(rendered) => {
                        log::debug!("rendered1: {:?}", rendered);
                        if let Some(prefix) = assistant_prefix {
                            Ok(format!("{}{}", rendered, prefix))
                        } else {
                            Ok(rendered)
                        }
                    },
                    Err(_) => {
                        // Both formats failed, return original error
                        Err(format!("Failed to apply chat template: {}", e))
                    },
                }
            } else {
                // String format failed, no fallback
                Err(format!("Failed to apply chat template: {}", e))
            }
        },
    }
}

/// Process content format for messages based on detected format
fn process_content_format(
    messages: &[ChatMessage],
    content_format: ChatTemplateContentFormat,
) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();

    for msg in messages {
        let mut msg_value = serde_json::to_value(msg).map_err(|e| format!("Failed to serialize message: {}", e))?;

        match content_format {
            ChatTemplateContentFormat::String => {
                // Convert content to string if it's an array
                if let Some(content) = msg_value.get("content") {
                    if content.is_array() {
                        let text_parts: Vec<String> = content
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
                            .collect();
                        let text = text_parts.join("");
                        msg_value["content"] = Value::String(text);
                    }
                }
            },
            ChatTemplateContentFormat::OpenAI => {
                // Ensure content is an array (OpenAI format)
                if let Some(content) = msg_value.get("content") {
                    if content.is_string() {
                        let text = content.as_str().unwrap_or("");
                        msg_value["content"] = Value::Array(vec![serde_json::json!({
                            "type": "text",
                            "text": text
                        })]);
                    }
                }
            },
        }

        result.push(msg_value);
    }

    Ok(result)
}

/// Process content format for messages, always converting to string format
/// Used as a fallback when OpenAI format detection is incorrect
fn process_content_format_string(messages: &[ChatMessage]) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();

    for msg in messages {
        let mut msg_value = serde_json::to_value(msg).map_err(|e| format!("Failed to serialize message: {}", e))?;

        // Always convert content to string format
        if let Some(content) = msg_value.get("content") {
            if content.is_array() {
                // Convert array content (OpenAI format) to string
                let text_parts: Vec<String> = content
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
                    .collect();
                let text = text_parts.join("");
                msg_value["content"] = Value::String(text);
            }
            // If content is already a string, leave it as is
        }

        result.push(msg_value);
    }

    Ok(result)
}

/// Process tool call arguments in messages
fn process_tool_call_arguments(messages: &mut [Value]) -> Result<(), String> {
    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str());
        if role != Some("assistant") {
            continue;
        }

        let Some(tool_calls) = msg.get_mut("tool_calls").and_then(|tc| tc.as_array_mut()) else {
            continue;
        };

        for call in tool_calls {
            let Some(function) = call.get_mut("function") else {
                continue;
            };
            let Some(args) = function.get_mut("arguments") else {
                continue;
            };
            let Some(args_str) = args.as_str() else {
                continue;
            };

            // Parse JSON string to object
            match serde_json::from_str::<Value>(args_str) {
                Ok(parsed) => *args = parsed,
                Err(e) => {
                    return Err(format!(
                        "Failed to parse tool call arguments: {}. Value: {}",
                        e, args_str
                    ));
                },
            }
        }
    }

    Ok(())
}
