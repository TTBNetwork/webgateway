use anyhow::Result;

fn get_totp_instance(
    username: impl Into<String>,
    secret: impl Into<Vec<u8>>,
) -> Result<totp_rs::TOTP> {
    Ok(totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        secret.into(),
        Some("WebGateway".to_string()),
        username.into(),
    )?)
}

pub fn get_totp_code(username: impl Into<String>, secret: impl Into<Vec<u8>>) -> Result<String> {
    Ok(get_totp_instance(username, secret)?.generate_current()?)
}

pub fn get_totp_url(username: impl Into<String>, secret: impl Into<Vec<u8>>) -> Result<String> {
    Ok(get_totp_instance(username, secret)?.get_url())
}
