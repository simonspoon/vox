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

/// Print a reply received from ask.
pub fn print_reply(reply: &Message, json_output: bool) {
    let text = reply.text.as_deref().unwrap_or("");
    if json_output {
        let obj = json!({
            "ok": true,
            "reply": text,
            "message_id": reply.message_id,
            "from": reply.from.as_ref().map(|u| &u.first_name),
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        print!("{text}");
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
