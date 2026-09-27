//! Credential reset must preserve account discovery without granting device trust.
//! Uses the real relay and crypto with declared synthetic Mac/client keys.
use super::*;

pub(super) async fn execute(driver: &Driver, after: bool) -> Result<()> {
    let stage = if after { "after" } else { "before" };
    let account = driver
        .http
        .get("http://relay:18081/v1/account")
        .bearer_auth(&driver.tokens[0])
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    let account_id = account["account"]["id"]
        .as_str()
        .context("active account id")?;
    ensure!(
        !account_id.is_empty() && account["account"]["status"] == "active",
        "account not active"
    );
    let directory = driver
        .http
        .get("http://relay:18081/v1/macs")
        .bearer_auth(&driver.tokens[0])
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    let mut macs = directory["macs"]
        .as_array()
        .context("Mac directory")?
        .iter()
        .map(|mac| mac["mac_id"].as_str().context("Mac ID"))
        .collect::<Result<Vec<_>>>()?;
    macs.sort_unstable();
    ensure!(
        macs == MACS[..2],
        "account directory changed or crossed ownership"
    );
    let state = json!({"account_id":account_id,"macs":macs});
    if after {
        let baseline: Value =
            serde_json::from_slice(&std::fs::read("/system/account-reset-state.json")?)?;
        ensure!(
            state == baseline,
            "password reset changed product identity or Mac directory"
        );
    } else {
        save("account-reset-state.json", state)?;
    }
    // Link 2 remains approved; initial() revoked link 0, and left work on Mac 1.
    // Use Mac 0 so this proof cannot accidentally consume that pending request.
    let link = 2;
    let request_id = format!("system-account-reset-{stage}");
    driver
        .submit(link, driver.envelope(link, 0, &request_id)?, 202)
        .await?;
    driver.complete(link, &request_id).await?;
    let status_path = format!("/v1/requests/{request_id}");
    let completed = driver
        .call(13, LINKS[link].0, Some(0), "GET", &status_path, vec![], 200)
        .await?;
    let response: EnvelopeV1 = serde_json::from_value(completed["response"].clone())?;
    ensure!(
        response
            .open(
                key(1).verifying_key(),
                &agreement(99),
                MACS[0],
                LINKS[link].0,
                MACS[0],
                now()
            )
            .is_err(),
        "fresh client agreement key decrypted an existing approved response"
    );

    for path in [&status_path, "/v1/changes?wait_s=0", "/v1/work?wait_s=0"] {
        let response = driver
            .http
            .get(format!("http://relay:18081{path}"))
            .bearer_auth(&driver.tokens[0])
            .send()
            .await?;
        ensure!(
            response.status().as_u16() == 401,
            "account token alone gained device access"
        );
    }
    // A valid account token plus a fresh key cannot impersonate an approved ID,
    // use an unregistered ID, or approve itself by impersonating the Mac.
    driver
        .call(99, LINKS[link].0, Some(0), "GET", &status_path, vec![], 401)
        .await?;
    driver
        .call(
            99,
            "system-reset-unapproved",
            Some(0),
            "GET",
            &status_path,
            vec![],
            401,
        )
        .await?;
    driver
        .call(
            99,
            LINKS[link].0,
            Some(0),
            "POST",
            "/v1/requests",
            driver.envelope(link, 0, &format!("system-reset-spoof-{stage}"))?,
            401,
        )
        .await?;
    let enrollment = json!({"device_id":"system-reset-unapproved","owner_sub":driver.fixture["owners"][0]["id"],"signing_key":public(99)});
    let response = driver
        .http
        .post("http://relay:18081/v1/devices")
        .bearer_auth(&driver.tokens[0])
        .json(&enrollment)
        .send()
        .await?;
    ensure!(
        response.status().as_u16() == 401,
        "account token self-approved a client"
    );
    driver
        .call(
            99,
            MACS[0],
            Some(0),
            "POST",
            "/v1/devices",
            serde_json::to_vec(&enrollment)?,
            401,
        )
        .await?;
    driver
        .submit(
            0,
            driver.envelope(0, 0, &format!("system-reset-revoked-{stage}"))?,
            401,
        )
        .await?;
    save(
        &format!("account-reset-{stage}.json"),
        json!({
            "phase":format!("account-reset-{stage}"),"status":"passed",
            "checks":["valid_account_directory","approved_link_encrypted_roundtrip",
                "fresh_agreement_key_cannot_decrypt","bearer_only_device_access_denied",
                "fresh_signing_key_cannot_impersonate_approved_link","unregistered_link_denied",
                "fresh_key_cannot_submit","account_cannot_self_approve","fresh_key_cannot_impersonate_mac",
                "revoked_link_remains_denied"]
        }),
    )
}
