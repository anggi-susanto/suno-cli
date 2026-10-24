use super::SunoClient;
use super::types::CaptchaCheckResponse;
use crate::errors::CliError;

impl SunoClient {
    /// Preflight Suno's captcha gate. `POST /api/c/check` with
    /// `{"ctype": "generation"}` returns `{"required": false}` for accounts
    /// above the trust threshold — letting callers skip the Chrome-piloted
    /// hCaptcha solve entirely. Verified live 2026-07-18.
    pub async fn captcha_check(&self, ctype: &str) -> Result<CaptchaCheckResponse, CliError> {
        let _timing = crate::timings::span("captcha.preflight");
        self.with_auth_retry(|| async {
            let resp = self
                .post("/api/c/check")
                .json(&serde_json::json!({ "ctype": ctype }))
                .send()
                .await?;
            let resp = self.check_response(resp).await?;
            Ok(resp.json().await?)
        })
        .await
    }
}
