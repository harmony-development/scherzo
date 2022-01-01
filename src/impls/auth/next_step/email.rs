use crate::impls::send_email;

use super::*;

const EMAIL_BODY_TEMPLATE_PLAIN: &str =
    include_str!("../../../../resources/email-token-template.txt");
const EMAIL_BODY_TEMPLATE_HTML: &str =
    include_str!("../../../../resources/email-token-template.html");

const LILLIES_SVG: &[u8] = include_bytes!("../../../../resources/lillies.svg");
const LOTUS_SVG: &[u8] = include_bytes!("../../../../resources/lotus.svg");

pub async fn send_token_email(
    deps: &Dependencies,
    to: &str,
    token: &str,
    action: &str,
) -> ServerResult<()> {
    let html_body = EMAIL_BODY_TEMPLATE_HTML
        .replace("{{action}}", action)
        .replace("{{token}}", token);
    let plain_body = EMAIL_BODY_TEMPLATE_PLAIN
        .replace("{{action}}", action)
        .replace("{{token}}", token);

    let files = vec![
        (
            LILLIES_SVG.to_vec(),
            "lillies".to_string(),
            "image/svg+xml".to_string(),
            true,
        ),
        (
            LOTUS_SVG.to_vec(),
            "lotus".to_string(),
            "image/svg+xml".to_string(),
            true,
        ),
    ];

    let subject = format!("Harmony - {} for {}", action, &deps.config.host);

    send_email(deps, to, subject, plain_body, Some(html_body), files).await?;

    Ok(())
}
