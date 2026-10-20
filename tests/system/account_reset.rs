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
    // Link 2 remains approved; initial() revoked link 0.
    let link = 2;
    driver.link_access(link, 0, 204).await?;
    let response = driver
        .http
        .delete("http://relay:18081/v1/iroh-node")
        .bearer_auth(&driver.tokens[0])
        .header("x-device-id", LINKS[link].0)
        .send()
        .await?;
    ensure!(
        response.status().as_u16() == 401,
        "account token alone gained device access"
    );
    // A valid account token plus a fresh key cannot impersonate an approved ID,
    // use an unregistered ID, or approve itself by impersonating the Mac.
    driver
        .call(99, LINKS[link].0, Some(0), "DELETE", "/v1/iroh-node", vec![], 401)
        .await?;
    driver
        .call(
            99,
            "system-reset-unapproved",
            Some(0),
            "DELETE",
            "/v1/iroh-node",
            vec![],
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
    driver.link_access(0, 0, 401).await?;
    save(
        &format!("account-reset-{stage}.json"),
        json!({
            "phase":format!("account-reset-{stage}"),"status":"passed",
            "checks":["valid_account_directory","approved_link_signed_access",
                "bearer_only_device_access_denied","fresh_signing_key_cannot_impersonate_approved_link",
                "unregistered_link_denied","account_cannot_self_approve","fresh_key_cannot_impersonate_mac",
                "revoked_link_remains_denied"]
        }),
    )
}
