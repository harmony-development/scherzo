use super::*;

pub const HELP_TEXT: &str = r#"
all commands should be prefixed with `/`.

commands are:
`generate registration-token` -> generates a registration token
`delete user <user_id>` -> deletes a user from the server
`help` -> shows help
"#;

pub struct AdminActionError;

#[derive(Debug, Clone, Copy)]
pub enum AdminAction {
    DeleteUser(u64),
    GenerateRegistrationToken,
    Help,
}

impl FromStr for AdminAction {
    type Err = AdminActionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let act = match s.strip_prefix('/').ok_or(AdminActionError)? {
            "generate registration-token" => AdminAction::GenerateRegistrationToken,
            "help" => AdminAction::Help,
            s => {
                if let Some(s) = s.strip_prefix("delete user") {
                    let user_id = s.trim().parse::<u64>().map_err(|_| AdminActionError)?;
                    AdminAction::DeleteUser(user_id)
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
        AdminAction::GenerateRegistrationToken => {
            let token = deps.auth_tree.generate_single_use_token([]).await?;
            Ok(token.into())
        }
        AdminAction::DeleteUser(user_id) => {
            auth::delete_user::logic(deps, user_id).await?;
            Ok(format!("deleted user {}", user_id))
        }
        AdminAction::Help => Ok(HELP_TEXT.to_string()),
    }
}
