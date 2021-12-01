use std::{error::Error, fmt::Display};

use scherzo::{config::DbConfig, impls::auth::AuthTree};

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<String>>();
    let db_path = std::env::var("SCHERZO_DB").unwrap_or_else(|_| "./db".to_string());

    let db = scherzo::db::open_db(db_path, DbConfig::default());

    let auth_tree = AuthTree::new(db.as_ref())?;

    match args.first().map(String::as_str).ok_or("no command")? {
        "list_accounts" => {
            for res in auth_tree.inner.iter() {
                let (key, _) = res?;

                if let Ok(parsed) = std::str::from_utf8(key.as_slice()) {
                    if parsed.contains('@') {
                        println!("email: {}", parsed);
                    }
                }
            }
        }
        _ => exit_with_msg("no such command", 1),
    }

    Ok(())
}

fn exit_with_msg(err: impl Display, code: i32) -> ! {
    eprintln!("error: {}", err);
    std::process::exit(code)
}
