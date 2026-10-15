//! The Mac's iroh endpoint against a running sandbox: the same envelope gets the same answer
//! over iroh as over the direct listener, and a device the Mac never paired is refused.
use std::time::Instant;

use anyhow::{Result, ensure};

use crate::{
    phone::{Link, Phone, new_request_id, probe},
    sandbox::{Sandbox, Target},
};

const ROUND_TRIPS: usize = 20;

/// `alondra-sandbox iroh`: pairs a phone directly with Mac 1, checks iroh and measures it.
pub fn run(sandbox: &Sandbox) -> Result<()> {
    let phone = Phone::open(sandbox, "iroh")?;
    let target = sandbox.mac(1)?;
    let link = match phone.direct_link(&target) {
        Ok(link) => link,
        Err(_) => phone.pair_direct(&target)?,
    };
    check(&phone, &target, &link)?;
    measure(&phone, &target, &link)
}

pub fn check(phone: &Phone, target: &Target, link: &Link) -> Result<()> {
    let endpoint = phone.mac_iroh_endpoint(target)?;
    let routes = phone.request_direct(target, "GET", "/routes", None, None)?;
    ensure!(
        routes.response["body"]["iroh_endpoint"] == endpoint.to_string().as_str(),
        "Mac {} gives one iroh endpoint in its hello and another in its routes: {}",
        target.number,
        routes.response
    );
    let session = phone.connect_iroh(target, link)?;
    let sealed = phone.seal(target, link, &new_request_id()?, &probe())?;
    let (status, direct_bytes) = phone.send_direct(target, &sealed)?;
    ensure!(status == 200, "the envelope sent directly got {status}");
    let through_iroh = session.send(&sealed)?;
    let directly: serde_json::Value = serde_json::from_slice(&direct_bytes)?;
    ensure!(through_iroh["response"] == directly, "iroh and the direct path answered the same envelope differently: {through_iroh}");
    let answer = phone.open_direct(target, link, &serde_json::to_vec(&through_iroh["response"])?)?;
    ensure!(answer["status"] == 200, "the request over iroh was answered {answer}");
    ensure!(session.send(&sealed)? == through_iroh, "a repeat over iroh got another answer");
    println!("ok: Mac {} answers the same envelope with the same bytes over iroh and directly", target.number);

    let stranger = phone.stranger()?;
    match phone.connect_iroh(target, &stranger) {
        Ok(_) => anyhow::bail!("Mac {} accepted an iroh hello from a device it never paired", target.number),
        Err(error) => ensure!(
            error.to_string().contains("\"unauthorized\""),
            "a stranger's iroh hello failed for another reason: {error:#}"
        ),
    }
    println!("ok: Mac {} refuses an iroh connection from a device it never paired", target.number);
    Ok(())
}

/// Median round trip of one read over iroh and over the direct listener, on this machine.
fn measure(phone: &Phone, target: &Target, link: &Link) -> Result<()> {
    let session = phone.connect_iroh(target, link)?;
    let (mut iroh, mut direct) = (Vec::new(), Vec::new());
    for _ in 0..ROUND_TRIPS {
        let sealed = phone.seal(target, link, &new_request_id()?, &probe())?;
        let started = Instant::now();
        ensure!(session.send(&sealed)?.get("response").is_some(), "a read over iroh was refused");
        iroh.push(started.elapsed().as_secs_f64() * 1000.0);
        let sealed = phone.seal(target, link, &new_request_id()?, &probe())?;
        let started = Instant::now();
        ensure!(phone.send_direct(target, &sealed)?.0 == 200, "a direct read was refused");
        direct.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "{ROUND_TRIPS} reads of /capabilities, median: iroh {:.1} ms, direct {:.1} ms (loopback)",
        median(&mut iroh),
        median(&mut direct)
    );
    Ok(())
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}
