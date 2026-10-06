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

    let target = sandbox.mac(1)?;
    let link = phone.link(&target)?;
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
        phone.request(&target, "GET", "/capabilities", None, None)?;
        println!("ok: revoked on Mac {count}, still served by Mac 1");
    }
    Ok(())
}
