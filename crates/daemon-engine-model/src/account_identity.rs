use super::Failed;
use alondra_core::accounts::{AccountBilling,AccountStatus};
use std::path::{Path,PathBuf};

const ACCOUNT:&str="native-account.json";
const PROOF:&str="native-auth-account.json";
/// The identity of each node, kept beside its host folders, under the node's name, so the
/// relaunch that retires a folder, or one that fails right after, cannot take it with it.
const KEPT:&str="native-identity";

#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAccountIdentity {
    email:Option<String>,
    pay_per_use:bool,
    #[serde(default,skip_serializing_if="Option::is_none")]
    native_identity:Option<String>,
    #[serde(skip)]
    native_auth:Option<NativeAuthAccount>,
}
#[derive(Clone,Debug,PartialEq,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct NativeAuthAccount{auth_method:String,email:Option<String>,workspace_id:Option<String>}
fn kept(directory:&Path)->Option<PathBuf>{Some(directory.parent()?.join(KEPT).join(directory.file_name()?))}
impl NativeAccountIdentity {
    pub fn can_resume_managed(&self)->bool{self.native_auth.as_ref().is_some_and(|actual|actual.auth_method=="chatgpt" && actual.email.as_ref().is_some_and(|email|!email.trim().is_empty()))}
    pub fn same_known_email(&self,current:&AccountStatus)->bool{
        self.email.as_deref().zip(current.email.as_deref()).is_some_and(|(before,after)|!after.trim().is_empty() && before.eq_ignore_ascii_case(after.trim()))
    }
    pub fn same_known_native_account(&self,provider:alondra_core::accounts::Provider,current:&AccountStatus)->bool{
        if provider!=alondra_core::accounts::Provider::Codex{return self.same_known_email(current)}
        self.native_auth.as_ref().is_some_and(|actual|actual.auth_method=="chatgpt"
            && actual.email.as_deref().zip(current.email.as_deref()).is_some_and(|(before,after)|!before.is_empty() && before.eq_ignore_ascii_case(after.trim()))
            && actual.workspace_id.as_deref().zip(current.native_identity.as_deref()).is_some_and(|(before,after)|!before.is_empty() && before==after)
            && self.change(current).is_none())
    }
    pub fn change(&self,current:&AccountStatus)->Option<&'static str>{
        if self.email.as_deref().zip(current.email.as_deref()).is_some_and(|(before,after)|!before.eq_ignore_ascii_case(after.trim())){Some("identity")}
        else if self.native_identity.as_deref().zip(current.native_identity.as_deref()).is_some_and(|(before,after)|before!=after){Some("identity")}
        else if self.pay_per_use!=matches!(current.billing,AccountBilling::PayPerUse){Some("billing policy")}else{None}
    }
    fn same_account(&self,other:&Self)->bool{self.email==other.email && self.pay_per_use==other.pay_per_use && self.native_identity==other.native_identity}
    pub fn has_proof(&self)->bool{self.native_auth.is_some()}
    fn files(&self)->Result<Vec<(&'static str,Vec<u8>)>,Failed>{
        let mut files=vec![(ACCOUNT,serde_json::to_vec(self).map_err(|error|Failed::new(format!("Encoding the native account identity: {error}")))?)];
        if let Some(actual)=&self.native_auth{files.push((PROOF,serde_json::to_vec(actual).map_err(|error|Failed::new(format!("Encoding the native account proof: {error}")))?));}
        Ok(files)
    }
    pub fn save(directory:&Path,status:Option<&AccountStatus>)->Result<(),Failed>{
        let Some(status)=status else{return Ok(())};
        let email=status.email.as_ref().filter(|email|!email.trim().is_empty() && email.len()<=1024).map(|email|email.trim().to_owned());
        let native_identity=status.native_identity.as_ref().filter(|identity|!identity.is_empty() && identity.len()<=1024).cloned();
        Self{email,pay_per_use:matches!(status.billing,AccountBilling::PayPerUse),native_identity,native_auth:None}.retain(directory)
    }
    /// The session in `directory` recorded `wrong`, another account's email, because its host ran
    /// on that account's profile: it takes `status`'s email, in the host's folder and where it is
    /// kept. Whether it changed anything.
    pub fn correct_email(directory:&Path,wrong:&str,status:&AccountStatus)->Result<bool,Failed>{
        let mut changed=false;
        for place in std::iter::once(directory.to_path_buf()).chain(kept(directory)){
            let Some(mut identity)=Self::read(&place)? else{continue};
            if !identity.email.as_deref().is_some_and(|email|email.eq_ignore_ascii_case(wrong)){continue}
            identity.email=status.email.as_ref().map(|email|email.trim().to_owned());
            let bytes=serde_json::to_vec(&identity).map_err(|error|Failed::new(format!("Encoding the corrected native identity: {error}")))?;
            replace_private(&place,ACCOUNT,&bytes)?;changed=true;
        }
        Ok(changed)
    }
    /// Writes this identity in a new host's folder, and where it is kept. A file the folder
    /// already holds with the same identity stays as it is; one with another refuses.
    pub fn retain(&self,directory:&Path)->Result<(),Failed>{
        use std::io::Write;use std::os::unix::fs::OpenOptionsExt;
        for (name,bytes) in self.files()?{
            match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(directory.join(name)){
                Ok(mut file)=>file.write_all(&bytes).and_then(|_|file.sync_all()).map_err(|error|Failed::written(format!("Syncing original native metadata: {error}"), &error))?,
                Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>{
                    let held=read_private(&directory.join(name),directory)?.and_then(|held|serde_json::from_slice::<serde_json::Value>(&held).ok());
                    if held!=serde_json::from_slice::<serde_json::Value>(&bytes).ok(){return Err(Failed::new(format!("This node's host folder already holds another account identity ({name})")))}
                }
                Err(error)=>return Err(Failed::written(format!("Keeping original native metadata: {error}"), &error)),
            }
        }
        std::fs::File::open(directory).and_then(|file|file.sync_all()).map_err(|error|Failed::written(format!("Syncing retained native metadata: {error}"), &error))?;
        self.keep(directory)
    }
    /// Keeps this identity of the node whose host folder is `directory` apart from that folder.
    pub fn keep(&self,directory:&Path)->Result<(),Failed>{
        let Some(place)=kept(directory) else{return Ok(())};
        if Self::read_observed(&place,alondra_core::accounts::Provider::Codex)?.is_some_and(|held|held.same_account(self) && (self.native_auth.is_none() || held.native_auth==self.native_auth)){return Ok(())}
        {use std::os::unix::fs::DirBuilderExt;std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&place).map_err(|error|Failed::written(format!("Keeping the native account identity: {error}"),&error))?;}
        for (name,bytes) in self.files()?{replace_private(&place,name,&bytes)?;}
        Ok(())
    }
    /// The identity of the node whose host folder is `directory`, from that folder or, when a
    /// relaunch left it without one, from where it is kept.
    pub fn load_observed(directory:&Path,provider:alondra_core::accounts::Provider)->Result<Option<Self>,Failed>{
        let here=Self::read_observed(directory,provider)?;
        if here.as_ref().is_some_and(|here|here.native_auth.is_some() || provider!=alondra_core::accounts::Provider::Codex){return Ok(here)}
        let Some(place)=kept(directory) else{return Ok(here)};
        let there=Self::read_observed(&place,provider)?;
        Ok(match (here,there){
            (None,there)=>there,
            (Some(mut here),Some(there)) if here.same_account(&there)=>{here.native_auth=there.native_auth;Some(here)}
            (here,_)=>here,
        })
    }
    /// As `load_observed`, and for a node that has no identity in either place, from the folders
    /// of its ended hosts, when they all agree on it. What it finds is kept for the next time.
    pub fn recover(directory:&Path,provider:alondra_core::accounts::Provider)->Result<Option<Self>,Failed>{
        let found=Self::load_observed(directory,provider)?;
        if found.as_ref().is_some_and(|found|found.native_auth.is_some() || provider!=alondra_core::accounts::Provider::Codex){return Ok(found)}
        let (Some(parent),Some(name))=(directory.parent(),directory.file_name().and_then(|name|name.to_str())) else{return Ok(found)};
        let ended=format!("{name}.ended-");
        let entries=match std::fs::read_dir(parent){Ok(entries)=>entries,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(found),Err(error)=>return Err(Failed::new(format!("Reading the ended hosts' folders: {error}")))};
        let mut archived:Vec<Self>=entries.filter_map(Result::ok).filter(|entry|entry.file_name().to_str().is_some_and(|archive|archive.starts_with(&ended)))
            .filter_map(|entry|Self::read_observed(&entry.path(),provider).ok().flatten())
            .filter(|archived|found.as_ref().is_none_or(|found|found.same_account(archived))).collect();
        if archived.iter().any(|archived|archived.native_auth.is_some()){archived.retain(|archived|archived.native_auth.is_some())}
        let Some(first)=archived.first().cloned() else{return Ok(found)};
        if archived.iter().any(|other|!other.same_account(&first) || other.native_auth!=first.native_auth){
            return Err(Failed::new("This node's ended hosts recorded more than one account identity, so Alondra cannot tell which account its session started on".into()))
        }
        first.keep(directory)?;
        Ok(Some(first))
    }
    pub fn load(directory:&Path)->Result<Option<Self>,Failed>{
        match Self::read(directory)?{Some(identity)=>Ok(Some(identity)),None=>kept(directory).map_or(Ok(None),|place|Self::read(&place))}
    }
    fn read_observed(directory:&Path,provider:alondra_core::accounts::Provider)->Result<Option<Self>,Failed>{
        let mut identity=Self::read(directory)?;
        if provider!=alondra_core::accounts::Provider::Codex{return Ok(identity)}
        let Some(expected)=identity.as_mut() else{return Ok(None)};
        let Some(bytes)=read_private(&directory.join(PROOF),directory).map_err(|error|Failed::new(format!("Reading native account proof: {}",error.why)))? else{return Ok(identity)};
        expected.native_auth=Some(serde_json::from_slice(&bytes).map_err(|error|Failed::new(format!("Decoding native account proof: {error}")))?);Ok(identity)
    }
    fn read(directory:&Path)->Result<Option<Self>,Failed>{
        let Some(bytes)=read_private(&directory.join(ACCOUNT),directory).map_err(|error|Failed::new(format!("Reading native account identity: {}",error.why)))? else{return Ok(None)};
        serde_json::from_slice(&bytes).map(Some).map_err(|error|Failed::new(format!("Decoding native account identity: {error}")))
    }
}

/// The bytes of `path`, a private file of `directory`'s owner, or none when it is missing.
fn read_private(path:&Path,directory:&Path)->Result<Option<Vec<u8>>,Failed>{
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::io::Read;
    let expected=match std::fs::symlink_metadata(path){Ok(metadata)=>metadata,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(Failed::new(format!("{error}")))};
    let owner=std::fs::metadata(directory).map_err(|error|Failed::new(format!("its folder: {error}")))?;
    if !expected.is_file() || expected.file_type().is_symlink() || expected.nlink()!=1 || expected.uid()!=owner.uid() || expected.mode()&0o777!=0o600 || expected.len()>4096{return Err(Failed::new("it is not a private file".into()))}
    let mut file=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path).map_err(|error|Failed::new(format!("{error}")))?;
    let actual=file.metadata().map_err(|error|Failed::new(format!("{error}")))?;
    if expected.ino()!=actual.ino() || expected.dev()!=actual.dev(){return Err(Failed::new("it changed while opening it".into()))}
    let mut bytes=vec![];file.by_ref().take(4097).read_to_end(&mut bytes).map_err(|error|Failed::new(format!("{error}")))?;
    if bytes.len()>4096{return Err(Failed::new("it is too large".into()))}
    Ok(Some(bytes))
}

/// Replaces `name` in `directory` with `bytes`, a private file, durably.
fn replace_private(directory:&Path,name:&str,bytes:&[u8])->Result<(),Failed>{
    use std::io::Write;use std::os::unix::fs::OpenOptionsExt;
    let staged=directory.join(format!("{name}.staged"));
    match std::fs::remove_file(&staged){Ok(())=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(Failed::written(format!("Clearing the staged native identity: {error}"),&error))}
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(&staged).map_err(|error|Failed::written(format!("Writing the native identity: {error}"),&error))?;
    file.write_all(bytes).and_then(|_|file.sync_all()).map_err(|error|Failed::written(format!("Syncing the native identity: {error}"),&error))?;
    std::fs::rename(&staged,directory.join(name)).map_err(|error|Failed::written(format!("Replacing the native identity: {error}"),&error))?;
    std::fs::File::open(directory).and_then(|folder|folder.sync_all()).map_err(|error|Failed::written(format!("Syncing the native identity folder: {error}"),&error))
}

#[cfg(test)]
mod tests{
    use super::*;
    fn status()->AccountStatus{AccountStatus{provider:alondra_core::accounts::Provider::Codex,id:alondra_core::accounts::AccountId::default_account(),name:"Native account".into(),state:alondra_core::accounts::AccountState::SignedIn,email:Some("native@example.test".into()),native_identity:None,plan:None,billing:AccountBilling::Subscription{plan:None,weight:None},sign_ins: 0, profile: None}}
    #[test]
    fn repeated_same_email_reauthentication_preserves_native_identity(){
        let directory=crate::test_support::Directory::new();std::fs::create_dir_all(&directory).unwrap();let mut account=status();
        NativeAccountIdentity::save(&directory,Some(&account)).unwrap();let expected=NativeAccountIdentity::load(&directory).unwrap().unwrap();
        for count in [1,2]{account.sign_ins=count;account.email=Some("NATIVE@example.test".into());account.billing=AccountBilling::Subscription{plan:Some("max".into()),weight:Some(2)};assert!(expected.same_known_email(&account));assert_eq!(expected.change(&account),None);}
        account.email=Some("other@example.test".into());assert!(!expected.same_known_email(&account));assert_eq!(expected.change(&account),Some("identity"));
    }
    #[test]
    fn a_session_recorded_with_another_accounts_email_takes_its_own_and_no_other_is_touched(){
        use std::os::unix::fs::PermissionsExt;
        let directory=crate::test_support::Directory::new();std::fs::create_dir_all(&directory).unwrap();let mut account=status();account.email=Some("other-profile@example.test".into());
        NativeAccountIdentity::save(&directory,Some(&account)).unwrap();account.email=Some("own-profile@example.test".into());
        assert_eq!(NativeAccountIdentity::load(&directory).unwrap().unwrap().change(&account),Some("identity"),"the guard refuses it before");
        assert!(!NativeAccountIdentity::correct_email(&directory,"third@example.test",&account).unwrap(),"an identity it did not record stays");
        assert_eq!(NativeAccountIdentity::load(&directory).unwrap().unwrap().change(&account),Some("identity"));
        assert!(NativeAccountIdentity::correct_email(&directory,"OTHER-profile@example.test",&account).unwrap());
        assert_eq!(NativeAccountIdentity::load(&directory).unwrap().unwrap().change(&account),None,"the guard accepts it after");
        assert_eq!(std::fs::metadata(directory.join("native-account.json")).unwrap().permissions().mode()&0o777,0o600);
        assert!(!directory.join("native-account.json.corrected").exists());
        assert!(!NativeAccountIdentity::correct_email(&directory,"other-profile@example.test",&account).unwrap(),"correcting again changes nothing");
        let empty=crate::test_support::Directory::new();std::fs::create_dir_all(&empty).unwrap();
        assert!(!NativeAccountIdentity::correct_email(&empty,"other-profile@example.test",&account).unwrap(),"a host with no record stays without one");
    }
    #[test]
    fn billing_policy_edits_are_not_presented_as_credential_identity_changes(){
        let directory=crate::test_support::Directory::new();std::fs::create_dir_all(&directory).unwrap();let mut account=status();
        NativeAccountIdentity::save(&directory,Some(&account)).unwrap();let expected=NativeAccountIdentity::load(&directory).unwrap().unwrap();
        account.billing=AccountBilling::PayPerUse;assert!(expected.same_known_email(&account));assert_eq!(expected.change(&account),Some("billing policy"));
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.join("native-account.json"),std::fs::Permissions::from_mode(0o644)).unwrap();assert!(NativeAccountIdentity::load(&directory).is_err());
    }
    #[test]
    fn same_email_cannot_recover_a_different_codex_workspace(){
        use alondra_core::accounts::Provider;
        let directory=crate::test_support::Directory::new();std::fs::create_dir_all(&directory).unwrap();let mut account=status();account.native_identity=Some("workspace-a".into());
        NativeAccountIdentity::save(&directory,Some(&account)).unwrap();let file=directory.join("native-auth-account.json");std::fs::write(&file,br#"{"authMethod":"chatgpt","email":"native@example.test","workspaceId":"workspace-a"}"#).unwrap();use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&file,std::fs::Permissions::from_mode(0o600)).unwrap();
        let expected=NativeAccountIdentity::load_observed(&directory,Provider::Codex).unwrap().unwrap();account.sign_ins=2;
        assert!(expected.same_known_native_account(Provider::Codex,&account));account.native_identity=Some("workspace-b".into());assert!(!expected.same_known_native_account(Provider::Codex,&account));assert_eq!(expected.change(&account),Some("identity"));
        assert!(expected.same_known_native_account(Provider::Claude,&account),"Claude same-email reauthentication remains compatible");
    }

    #[test]
    fn cached_chatgpt_roster_labels_cannot_make_an_api_key_execution_a_managed_recovery_proof(){
        use alondra_core::accounts::Provider;use std::os::unix::fs::PermissionsExt;
        let directory=crate::test_support::Directory::new();std::fs::create_dir_all(&directory).unwrap();let mut account=status();account.native_identity=Some("old-managed-workspace".into());account.billing=AccountBilling::PayPerUse;
        NativeAccountIdentity::save(&directory,Some(&account)).unwrap();let path=directory.join("native-auth-account.json");std::fs::write(&path,br#"{"authMethod":"apikey","email":null,"workspaceId":null}"#).unwrap();std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o600)).unwrap();
        let expected=NativeAccountIdentity::load_observed(&directory,Provider::Codex).unwrap().unwrap();assert!(expected.same_known_email(&account));assert!(!expected.same_known_native_account(Provider::Codex,&account));
    }

    fn proof(directory:&Path,bytes:&[u8]){use std::os::unix::fs::PermissionsExt;let path=directory.join("native-auth-account.json");std::fs::write(&path,bytes).unwrap();std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o600)).unwrap();}
    fn host_folder(root:&Path,name:&str)->PathBuf{use std::os::unix::fs::DirBuilderExt;let path=root.join(name);std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();path}

    #[test]
    fn retaining_the_same_identity_again_keeps_it_and_another_one_refuses(){
        use alondra_core::accounts::Provider;
        let root=crate::test_support::Directory::new();std::fs::create_dir_all(&root).unwrap();let folder=host_folder(&root,"node");let mut account=status();
        NativeAccountIdentity::save(&folder,Some(&account)).unwrap();proof(&folder,br#"{"authMethod":"chatgpt","email":"native@example.test","workspaceId":null}"#);
        let identity=NativeAccountIdentity::load_observed(&folder,Provider::Codex).unwrap().unwrap();
        identity.retain(&folder).unwrap();NativeAccountIdentity::save(&folder,Some(&account)).unwrap();
        account.email=Some("other@example.test".into());
        assert!(NativeAccountIdentity::save(&folder,Some(&account)).is_err(),"a folder never takes another account's identity");
    }

    #[test]
    fn a_folder_without_an_identity_reads_the_kept_one_and_ended_hosts_must_agree(){
        use alondra_core::accounts::Provider;
        let root=crate::test_support::Directory::new();std::fs::create_dir_all(&root).unwrap();let account=status();
        let first=host_folder(&root,"node.ended-first");NativeAccountIdentity::save(&first,Some(&account)).unwrap();proof(&first,br#"{"authMethod":"chatgpt","email":"native@example.test","workspaceId":null}"#);
        std::fs::remove_dir_all(root.join("native-identity")).unwrap();host_folder(&root,"node.ended-empty");let folder=host_folder(&root,"node");
        assert!(NativeAccountIdentity::load_observed(&folder,Provider::Codex).unwrap().is_none(),"reading never walks the ended hosts");
        assert!(NativeAccountIdentity::recover(&folder,Provider::Codex).unwrap().unwrap().has_proof());
        std::fs::remove_dir_all(&first).unwrap();
        assert!(NativeAccountIdentity::load_observed(&folder,Provider::Codex).unwrap().unwrap().has_proof(),"what it recovered is kept");
        std::fs::remove_dir_all(root.join("native-identity")).unwrap();
        for (name,email) in [("node.ended-a","native@example.test"),("node.ended-b","other@example.test")]{let mut other=status();other.email=Some(email.into());let archived=host_folder(&root,name);NativeAccountIdentity::save(&archived,Some(&other)).unwrap();}
        std::fs::remove_dir_all(root.join("native-identity")).unwrap();
        assert!(NativeAccountIdentity::recover(&folder,Provider::Codex).is_err(),"ended hosts that disagree prove no identity");
    }

    #[test]
    fn original_nonsecret_account_metadata_survives_retiring_the_host_directory(){
        use std::os::unix::fs::PermissionsExt;use alondra_core::accounts::Provider;
        let directory=crate::test_support::Directory::new();let old=directory.join("old");let next=directory.join("next");std::fs::create_dir_all(&old).unwrap();std::fs::create_dir_all(&next).unwrap();let mut account=status();account.native_identity=Some("workspace-a".into());NativeAccountIdentity::save(&old,Some(&account)).unwrap();let path=old.join("native-auth-account.json");std::fs::write(&path,br#"{"authMethod":"chatgpt","email":"native@example.test","workspaceId":"workspace-a"}"#).unwrap();std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o600)).unwrap();
        let expected=NativeAccountIdentity::load_observed(&old,Provider::Codex).unwrap().unwrap();expected.retain(&next).unwrap();let restored=NativeAccountIdentity::load_observed(&next,Provider::Codex).unwrap().unwrap();assert!(restored.same_known_native_account(Provider::Codex,&account));assert_eq!(std::fs::read(old.join("native-auth-account.json")).unwrap(),std::fs::read(next.join("native-auth-account.json")).unwrap());
    }

}
