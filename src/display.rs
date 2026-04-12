use crate::telegram::{Message, User};
use serde_json::json;

/// Print the result of a sent message.
pub fn print_sent(message: &Message, json_output: bool) {
    if json_output {
        let obj = json!({
            "ok": true,
            "message_id": message.message_id,
            "chat_id": message.chat.id,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        println!("Message sent (id: {})", message.message_id);
    }
}

/// Print a reply received from ask. If `file` is Some, the reply carried a
/// document or photo that has been downloaded to the given local path.
pub fn print_reply(
    reply: &Message,
    file: Option<&std::path::Path>,
    json_output: bool,
    timed_out: bool,
) {
    let text = reply.text.as_deref().unwrap_or("");
    if json_output {
        let mut obj = json!({
            "ok": true,
            "message_id": reply.message_id,
            "from": reply.from.as_ref().map(|u| &u.first_name),
        });
        if let Some(path) = file {
            obj["file"] = json!(path.to_string_lossy());
        } else {
            obj["reply"] = json!(text);
        }
        if timed_out {
            obj["timed_out"] = json!(true);
        }
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else if let Some(path) = file {
        print!("{}", path.display());
    } else {
        print!("{text}");
    }
}

/// Print a default reply value when ask times out.
pub fn print_reply_default(default: &str, json_output: bool) {
    if json_output {
        let obj = json!({
            "ok": true,
            "reply": default,
            "timed_out": true,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        print!("{default}");
    }
}

/// Print bot status info.
pub fn print_status(user: &User, json_output: bool) {
    if json_output {
        let obj = json!({
            "ok": true,
            "id": user.id,
            "is_bot": user.is_bot,
            "first_name": user.first_name,
            "username": user.username,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        println!(
            "Bot: {} (@{})",
            user.first_name,
            user.username.as_deref().unwrap_or("unknown")
        );
        println!("ID:  {}", user.id);
        println!("OK:  connected");
    }
}

/// Print a confirm result.
pub fn print_confirm(answer: &str, message_id: i64, json_output: bool, timed_out: bool) {
    if json_output {
        let mut obj = json!({
            "ok": true,
            "answer": answer,
            "message_id": message_id,
        });
        if timed_out {
            obj["timed_out"] = json!(true);
        }
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        print!("{answer}");
    }
}

/// Print a pick result.
pub fn print_pick(choice: &str, message_id: i64, json_output: bool, timed_out: bool) {
    if json_output {
        let mut obj = json!({
            "ok": true,
            "choice": choice,
            "message_id": message_id,
        });
        if timed_out {
            obj["timed_out"] = json!(true);
        }
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        print!("{choice}");
    }
}

/// Print a wait result when a message is received. If `file` is Some, the
/// message carried a document or photo that has been downloaded locally.
pub fn print_wait_result(
    text: &str,
    file: Option<&std::path::Path>,
    message_id: i64,
    from_name: &str,
    json_output: bool,
) {
    if json_output {
        let mut obj = json!({
            "ok": true,
            "message_id": message_id,
            "from": from_name,
            "timed_out": false,
        });
        if let Some(path) = file {
            obj["file"] = json!(path.to_string_lossy());
        } else {
            obj["message"] = json!(text);
        }
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else if let Some(path) = file {
        print!("{}", path.display());
    } else {
        print!("{text}");
    }
}

/// Print a default value when wait times out.
pub fn print_wait_default(text: &str, json_output: bool) {
    if json_output {
        let obj = json!({
            "ok": true,
            "message": text,
            "timed_out": true,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        print!("{text}");
    }
}

/// Print the result of a sent file.
pub fn print_file_sent(message: &Message, filename: &str, json_output: bool) {
    if json_output {
        let obj = json!({
            "ok": true,
            "message_id": message.message_id,
            "filename": filename,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        println!("File sent: {} (id: {})", filename, message.message_id);
    }
}

/// Print the result of an edited (progress) message.
pub fn print_progress(message: &Message, json_output: bool) {
    if json_output {
        let obj = json!({
            "ok": true,
            "message_id": message.message_id,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        println!("Updated message {}", message.message_id);
    }
}

/// Print init success.
pub fn print_init_success(user: &User) {
    println!(
        "Initialized vox with bot @{} ({})",
        user.username.as_deref().unwrap_or("unknown"),
        user.first_name
    );
    println!("Token stored in macOS Keychain.");
    println!("Config saved to ~/.config/vox/config.toml");
}
