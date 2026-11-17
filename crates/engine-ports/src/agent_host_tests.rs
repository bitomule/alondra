use super::*;
use crate::test_support::Directory;
use std::sync::Mutex;

fn written(directory: &Directory, name: &str, bytes: &[u8]) -> (PathBuf, String) {
    let path = directory.join(name);
    std::fs::write(&path, bytes).unwrap();
    let digest = alondra_foundation::artifact_digest::digest(&path).unwrap();
    (path, digest)
}

fn directory() -> Directory {
    let directory = Directory::new();
    std::fs::create_dir_all(&*directory).unwrap();
    directory
}

#[test]
fn an_artifact_verifies_only_against_the_digests_of_its_files() {
    let directory = directory();
    let (host, host_digest) = written(&directory, "host", b"host");
    let (artifact, artifact_digest) = written(&directory, "artifact", b"artifact");
    assert!(VerifiedArtifact::verify(&host, &artifact, &host_digest, &artifact_digest).is_ok());
    let refused = VerifiedArtifact::verify(&host, &artifact, &artifact_digest, &artifact_digest).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    assert!(VerifiedArtifact::verify(&host, &artifact, "not a digest", &artifact_digest).is_err());
}

#[test]
fn a_verified_artifact_replaced_with_its_old_modification_time_no_longer_revalidates() {
    let directory = directory();
    let (path, expected) = written(&directory, "host", b"good");
    let verified = VerifiedArtifact::verify(&path, &path, &expected, &expected).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, b"evil").unwrap();
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(modified).unwrap();
    assert!(verified.revalidate().is_err());
    assert!(VerifiedArtifact::verify(&path, &path, &expected, &expected).is_err());
}

struct Recorded {
    identity: LaunchIdentity,
}
impl LaunchedHost for Recorded {
    fn identity(&self) -> &LaunchIdentity {
        &self.identity
    }
    fn wait_for_socket(&mut self, _: Instant) -> io::Result<UnixStream> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn observed_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(None)
    }
    fn process_id(&self) -> u32 {
        7
    }
    fn stopper(&self) -> HostStop {
        unimplemented!()
    }
    fn diagnostic_tail(&self) -> Vec<DiagnosticCode> {
        Vec::new()
    }
    fn wait(&mut self) -> io::Result<ExitStatus> {
        Err(io::ErrorKind::Unsupported.into())
    }
}
struct Endpoint(HostFolder);
impl AdoptedEndpoint for Endpoint {
    fn identity(&self) -> &HostFolder {
        &self.0
    }
    fn connect(&self, _: Instant) -> io::Result<UnixStream> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn stopper(&self) -> HostStop {
        unimplemented!()
    }
    fn stop_with_anchor(&mut self, _: Option<Member>, _: bool) -> io::Result<()> {
        Ok(())
    }
    fn stop(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Default)]
struct Hosts {
    bootstraps: Mutex<Vec<Vec<u8>>>,
}
impl AgentHost for Hosts {
    fn launch(&self, launch: HostLaunch<'_>, build: &mut dyn FnMut(&LaunchIdentity) -> io::Result<Vec<u8>>) -> io::Result<Box<dyn LaunchedHost>> {
        let identity = LaunchIdentity::new(HostFolder::at(launch.state_dir.to_path_buf()), launch.artifact);
        self.bootstraps.lock().unwrap().push(build(&identity)?);
        assert!(build(&identity).is_err(), "a bootstrap is built once");
        Ok(Box::new(Recorded { identity }))
    }
    fn adopt(&self, state_dir: &Path, inspect: &mut dyn FnMut(&HostFolder, &[u8]) -> io::Result<()>) -> io::Result<Box<dyn AdoptedEndpoint>> {
        let folder = HostFolder::at(state_dir.to_path_buf());
        inspect(&folder, b"bootstrap")?;
        Ok(Box::new(Endpoint(folder)))
    }
    fn claim(&self, _: &Path) -> io::Result<Retained> {
        Ok(Retained::Absent)
    }
    fn end_holders(&self, _: &Path) -> io::Result<Retained> {
        Ok(Retained::Absent)
    }
    fn run(&self, _: ProbeRun) -> io::Result<ProbeOutput> {
        Ok(ProbeOutput::default())
    }
}

#[test]
fn a_launch_builds_its_bootstrap_with_its_owner_and_keeps_the_owner() {
    let directory = directory();
    let (path, digest) = written(&directory, "host", b"host");
    let artifact = VerifiedArtifact::verify(&path, &path, &digest, &digest).unwrap();
    let hosts = Hosts::default();
    let host = OwnedHost::launch(&hosts, "owner", HostLaunch { artifact, state_dir: &directory, environment: &BTreeMap::new(), confinement: None }, |owner, identity| {
        Ok(format!("{owner} in {}", identity.state_dir().display()).into_bytes())
    })
    .unwrap();
    assert_eq!(*host.owner(), "owner");
    assert_eq!(host.process_id(), 7);
    assert_eq!(hosts.bootstraps.lock().unwrap().clone(), vec![format!("owner in {}", directory.display()).into_bytes()]);
}

#[test]
fn an_adoption_returns_what_the_inspection_of_its_bootstrap_found() {
    let directory = directory();
    let (host, found) = AdoptedHost::adopt(&Hosts::default(), 3, &directory, |owner, folder, bytes| {
        Ok((*owner, folder.socket_path().to_path_buf(), bytes.to_vec()))
    })
    .unwrap();
    assert_eq!(found, (3, directory.join("s"), b"bootstrap".to_vec()));
    assert_eq!(*host.owner(), 3);
    assert!(AdoptedHost::adopt(&Hosts::default(), (), &directory, |_, _, _| Err::<(), _>(io::Error::other("refused"))).is_err());
}
