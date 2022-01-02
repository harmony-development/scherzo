use super::*;

pub const HELP_TEXT: &str = r#"
all commands should be prefixed with `/`.

commands are:
`generate token` -> generates a single use token
`delete user <user_id>` -> deletes a user from the server
`set motd <new_motd>` -> sets the server MOTD
`help` -> shows help
"#;

pub struct AdminActionError;

#[derive(Debug, Clone)]
pub enum AdminAction {
    SetMotd(String),
    DeleteUser(u64),
    GenerateToken,
    Help,
}

impl FromStr for AdminAction {
    type Err = AdminActionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let act = match s.strip_prefix('/').ok_or(AdminActionError)? {
            "generate token" => AdminAction::GenerateToken,
            "help" => AdminAction::Help,
            s => {
                if let Some(s) = s.strip_prefix("delete user") {
                    let user_id = s.trim().parse::<u64>().map_err(|_| AdminActionError)?;
                    AdminAction::DeleteUser(user_id)
                } else if let Some(s) = s.strip_prefix("set motd") {
                    let new_motd = s.trim().to_string();
                    AdminAction::SetMotd(new_motd)
                } else {
                    return Err(AdminActionError);
                }
            }
        };
        Ok(act)
    }
}

pub async fn run_str(deps: &Dependencies, action: &str) -> ServerResult<String> {
    let maybe_action = AdminAction::from_str(action);
    match maybe_action {
        Ok(action) => run(deps, action).await,
        Err(_) => Ok(format!("invalid command: `{}`", action)),
    }
}

pub async fn run(deps: &Dependencies, action: AdminAction) -> ServerResult<String> {
    match action {
        AdminAction::GenerateToken => {
            let token = deps.auth_tree.generate_single_use_token([]).await?;
            Ok(token.into())
        }
        AdminAction::DeleteUser(user_id) => {
            auth::delete_user::logic(deps, user_id).await?;
            Ok(format!("deleted user {}", user_id))
        }
        AdminAction::SetMotd(new_motd) => {
            deps.runtime_config.lock().motd = new_motd;
            Ok("new MOTD set".to_string())
        }
        AdminAction::Help => Ok(HELP_TEXT.to_string()),
    }
}
