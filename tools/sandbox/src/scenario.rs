//! The automatic checks `//tests/sandbox:scenarios` runs against a fresh sandbox.
use anyhow::{Result, bail, ensure};

use crate::{
    phone::{Phone, Submitted, new_request_id, probe},
    sandbox::Sandbox,
};

pub fn run(sandbox: &Sandbox) -> Result<()> {
    let phone = Phone::open(sandbox, "scenario")?;
    let count = sandbox.mac_count();

    for number in 1..=count {
        let target = sandbox.mac(number)?;
        phone.pair(&target)?;
        let reply = phone.request(&target, "GET", "/capabilities", None, None)?;
        ensure!(
            reply.response.get("error").is_none(),
            "Mac {number} answered /capabilities with an error: {}",
            reply.response
        );
        println!("ok: Mac {number} answers the paired phone through the relay");
    }

    for number in 1..=count {
        let target = sandbox.mac(number)?;
        phone.pair_direct(&target)?;
        let reply = phone.request_direct(&target, "GET", "/capabilities", None, None)?;
        ensure!(
            reply.response.get("error").is_none(),
            "Mac {number} answered /capabilities directly with an error: {}",
            reply.response
        );
        let routes = phone.request_direct(&target, "GET", "/routes", None, None)?;
        ensure!(
            routes.response["status"] == 200 && routes.response["body"]["port"].as_u64().is_some(),
            "Mac {number} did not tell its routes: {}",
            routes.response
        );
        println!("ok: Mac {number} pairs directly (same code on both sides), answers without the relay and tells its routes");
    }

    let target = sandbox.mac(1)?;
    let relay_link = phone.link(&target)?;
    let request_id = new_request_id()?;
    let sealed = phone.seal(&target, &relay_link, &request_id, &probe())?;
    ensure!(matches!(phone.submit(&relay_link, &sealed)?, Submitted::Accepted), "relay send refused");
    let (through_relay, _) = phone.response_envelope(&target, &relay_link, &request_id)?;
    let (status, direct_bytes) = phone.send_direct(&target, &sealed)?;
    ensure!(status == 200, "the same envelope sent directly got {status}");
    let directly: serde_json::Value = serde_json::from_slice(&direct_bytes)?;
    ensure!(through_relay == directly, "the relay and the direct path answered the same envelope differently");
    println!("ok: the same envelope gets the same answer through the relay and directly");

    let direct_link = phone.direct_link(&target)?;
    let request_id = new_request_id()?;
    let sealed = phone.seal(&target, &direct_link, &request_id, &probe())?;
    let first = phone.send_direct(&target, &sealed)?;
    let again = phone.send_direct(&target, &sealed)?;
    ensure!(first.0 == 200 && first == again, "a repeated direct request got a different response");
    let other = phone.send_direct(&target, &phone.seal(&target, &direct_link, &request_id, &probe())?)?;
    ensure!(other.0 == 409, "other bytes under a used request ID got {} directly", other.0);
    for path in ["/devices/pending", "/tasks", "/daemon"] {
        let status = phone.direct_status(&target, path)?;
        ensure!(status == 404, "{path} answered {status} on the direct port");
    }
    println!("ok: direct repeats replay, conflicts are refused, the loopback API is not on the direct port");

    crate::iroh_check::check(&phone, &target, &direct_link)?;

    if count >= 2 {
        let other_mac = sandbox.mac(2)?;
        let misdirected = phone.seal(&target, &direct_link, &new_request_id()?, &probe())?;
        let (status, _) = phone.send_direct(&other_mac, &misdirected)?;
        ensure!(status == 421, "an envelope for Mac 1 got {status} on Mac 2");
        println!("ok: an envelope for Mac 1 is refused by Mac 2");
    }

    let denied = phone.start_direct(&target)?;
    ensure!(phone.code_on_mac(&target, &denied)? == denied.code, "codes differ for the denied attempt");
    phone.decide_on_mac(&target, &denied, false)?;
    ensure!(phone.pairing_state(&target, &denied)? == "denied", "the denied attempt is not denied");
    let status = phone.repeat_start(&target, &denied)?;
    ensure!(status == 403, "denied keys asked again and got {status}");
    println!("ok: a denied phone cannot ask again with the same keys");

    target.mac.revoke_device(&target.local, &direct_link.device_id)?;
    let (status, reply) = phone.send_direct(&target, &phone.seal(&target, &direct_link, &new_request_id()?, &probe())?)?;
    ensure!(status == 200, "a revoked direct phone got {status} instead of its sealed answer");
    let told = phone.open_direct(&target, &direct_link, &reply)?;
    ensure!(told["status"] == 403 && told["error"] == "device_revoked", "a revoked direct phone was told {told}");
    println!("ok: revoking the direct phone on the Mac cuts it off at once and tells it so, sealed");

    let link = relay_link;
    let request_id = new_request_id()?;
    let sealed = phone.seal(&target, &link, &request_id, &probe())?;
    ensure!(matches!(phone.submit(&link, &sealed)?, Submitted::Accepted), "first send refused");
    let (first, _) = phone.response_envelope(&target, &link, &request_id)?;
    ensure!(matches!(phone.submit(&link, &sealed)?, Submitted::Accepted), "repeated send refused");
    let (again, _) = phone.response_envelope(&target, &link, &request_id)?;
    ensure!(first == again, "a repeated request got a different response");
    match phone.submit(&link, &phone.seal(&target, &link, &request_id, &probe())?)? {
        Submitted::Refused(409, _) => {}
        Submitted::Refused(status, reply) => bail!("other bytes under a used request ID: {status} {reply}"),
        Submitted::Accepted => bail!("other bytes under a used request ID were accepted"),
    }
    println!("ok: a repeated request gets the stored response; other bytes under its ID conflict");

    if count >= 2 {
        let revoked = sandbox.mac(count)?;
        phone.revoke(&revoked)?;
        let link = phone.link(&revoked)?;
        match phone.submit(&link, &phone.seal(&revoked, &link, &new_request_id()?, &probe())?)? {
            Submitted::Refused(401, _) => {}
            Submitted::Refused(status, reply) => bail!("revoked phone got {status}: {reply}"),
            Submitted::Accepted => bail!("revoked phone was accepted"),
        }
        let (status, reply) = phone.send_direct(&revoked, &phone.seal(&revoked, &link, &new_request_id()?, &probe())?)?;
        ensure!(status == 200, "the revoked phone got {status} on the direct port instead of its sealed answer");
        let told = phone.open_direct(&revoked, &link, &reply)?;
        ensure!(told["error"] == "device_revoked", "the revoked phone was told {told} on the direct port");
        phone.request(&target, "GET", "/capabilities", None, None)?;
        println!("ok: revoked on Mac {count} through the relay and directly, still served by Mac 1");
    }
    Ok(())
}
