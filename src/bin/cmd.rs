use std::{error::Error, fmt::Display, io::prelude::*, mem::size_of};

use scherzo::{
    config::DbConfig,
    db::deser_guild,
    impls::{auth::AuthTree, chat::ChatTree},
};

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<String>>();
    let db_path = std::env::var("SCHERZO_DB").unwrap_or_else(|_| "./db".to_string());

    let db = scherzo::db::open_db(db_path, DbConfig::default());

    let auth_tree = AuthTree::new(db.as_ref())?;
    let chat_tree = ChatTree::new(db.as_ref())?;

    match args.first().map(String::as_str).ok_or("no command")? {
        "list" => match args.get(1).map(String::as_str).ok_or("need list name")? {
            "accounts" => {
                let mut stdout = std::io::stdout();
                for res in auth_tree.inner.iter() {
                    let (key, _) = res?;

                    if let Ok(parsed) = std::str::from_utf8(key.as_slice()) {
                        if parsed.contains('@') {
                            writeln!(stdout, "email: {}", parsed)?;
                        }
                    }
                }
            }
            "guilds" => {
                let mut stdout = std::io::stdout();
                for res in chat_tree.chat_tree.iter() {
                    let (key, val) = res?;

                    if key.len() == size_of::<u64>() {
                        let guild = deser_guild(val);
                        let id =
                            u64::from_be_bytes(key.try_into().expect("failed to convert to id"));
                        writeln!(stdout, "{}: {:#?}", id, guild)?;
                    }
                }
            }
            "channels" => {
                let guild_id = args
                    .get(2)
                    .map(String::as_str)
                    .ok_or("need guild id")?
                    .parse::<u64>()?;
                let channels = chat_tree.get_guild_channels_logic(guild_id, 0)?;

                let mut stdout = std::io::stdout();
                for channel in channels.channels {
                    writeln!(
                        stdout,
                        "{}: {:#?}",
                        channel.channel_id,
                        channel
                            .channel
                            .ok_or("channel doesnt have channel object")?
                    )?;
                }
            }
            _ => exit_with_msg("no such list", 1),
        },
        _ => exit_with_msg("no such command", 1),
    }

    Ok(())
}

fn exit_with_msg(err: impl Display, code: i32) -> ! {
    eprintln!("error: {}", err);
    std::process::exit(code)
}
