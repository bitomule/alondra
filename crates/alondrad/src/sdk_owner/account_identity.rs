use super::Failed;
use alondra_core::accounts::{AccountBilling,AccountStatus};
use std::path::Path;

#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeAccountIdentity {
    email:Option<String>,
    pay_per_use:bool,
    #[serde(default,skip_serializing_if="Option::is_none")]
    native_identity:Option<String>,
    #[serde(skip)]
    native_auth:Option<NativeAuthAccount>,
}
#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct NativeAuthAccount{auth_method:String,email:Option<String>,workspace_id:Option<String>}
impl NativeAccountIdentity {
    pub(crate) fn can_resume_managed(&self)->bool{self.native_auth.as_ref().is_some_and(|actual|actual.auth_method=="chatgpt" && actual.email.as_ref().is_some_and(|email|!email.trim().is_empty()))}
    pub(crate) fn same_known_email(&self,current:&AccountStatus)->bool{
        self.email.as_deref().zip(current.email.as_deref()).is_some_and(|(before,after)|!after.trim().is_empty() && before.eq_ignore_ascii_case(after.trim()))
    }
    pub(crate) fn same_known_native_account(&self,provider:alondra_core::accounts::Provider,current:&AccountStatus)->bool{
        if provider!=alondra_core::accounts::Provider::Codex{return self.same_known_email(current)}
        self.native_auth.as_ref().is_some_and(|actual|actual.auth_method=="chatgpt"
            && actual.email.as_deref().zip(current.email.as_deref()).is_some_and(|(before,after)|!before.is_empty() && before.eq_ignore_ascii_case(after.trim()))
            && actual.workspace_id.as_deref().zip(current.native_identity.as_deref()).is_some_and(|(before,after)|!before.is_empty() && before==after)
            && self.change(current).is_none())
    }
    pub(crate) fn change(&self,current:&AccountStatus)->Option<&'static str>{
        if self.email.as_deref().zip(current.email.as_deref()).is_some_and(|(before,after)|!before.eq_ignore_ascii_case(after.trim())){Some("identity")}
        else if self.native_identity.as_deref().zip(current.native_identity.as_deref()).is_some_and(|(before,after)|before!=after){Some("identity")}
        else if self.pay_per_use!=matches!(current.billing,AccountBilling::PayPerUse){Some("billing policy")}else{None}
    }
    pub(crate) fn save(directory:&Path,status:Option<&AccountStatus>)->Result<(),Failed>{
        let Some(status)=status else{return Ok(())};
        let email=status.email.as_ref().filter(|email|!email.trim().is_empty() && email.len()<=1024).map(|email|email.trim().to_owned());
        let native_identity=status.native_identity.as_ref().filter(|identity|!identity.is_empty() && identity.len()<=1024).cloned();
        let identity=Self{email,pay_per_use:matches!(status.billing,AccountBilling::PayPerUse),native_identity,native_auth:None};
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;
        let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(directory.join("native-account.json")).map_err(|error|Failed::written(format!("Keeping the native account identity: {error}"), &error))?;
        let bytes=serde_json::to_vec(&identity).map_err(|error|Failed::new(format!("Encoding the native account identity: {error}")))?;
        file.write_all(&bytes).and_then(|_|file.sync_all()).map_err(|error|Failed::written(format!("Syncing the native account identity: {error}"), &error))?;
        std::fs::File::open(directory).and_then(|folder|folder.sync_all()).map_err(|error|Failed::written(format!("Syncing the native account folder: {error}"), &error))
    }
    pub(crate) fn retain(&self,directory:&Path)->Result<(),Failed>{
        use std::io::Write;use std::os::unix::fs::OpenOptionsExt;
        let mut files=vec![("native-account.json",serde_json::to_vec(self).map_err(|error|Failed::new(format!("Encoding retained native identity: {error}")))?)];
        if let Some(actual)=&self.native_auth{files.push(("native-auth-account.json",serde_json::to_vec(actual).map_err(|error|Failed::new(format!("Encoding retained native proof: {error}")))?));}
        for (name,bytes) in files{let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(directory.join(name)).map_err(|error|Failed::written(format!("Keeping original native metadata: {error}"), &error))?;file.write_all(&bytes).and_then(|_|file.sync_all()).map_err(|error|Failed::written(format!("Syncing original native metadata: {error}"), &error))?;}
        std::fs::File::open(directory).and_then(|file|file.sync_all()).map_err(|error|Failed::written(format!("Syncing retained native metadata: {error}"), &error))
    }
    pub(crate) fn load_observed(directory:&Path,provider:alondra_core::accounts::Provider)->Result<Option<Self>,Failed>{
        let mut identity=Self::load(directory)?;
        if provider!=alondra_core::accounts::Provider::Codex{return Ok(identity)}
        let Some(expected)=identity.as_mut() else{return Ok(None)};
        use std::os::unix::fs::{MetadataExt,OpenOptionsExt};use std::io::Read;
        let path=directory.join("native-auth-account.json");
        let before=match std::fs::symlink_metadata(&path){Ok(meta)=>meta,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(identity),Err(error)=>return Err(Failed::new(format!("Reading native account proof: {error}")))};
        let owner=std::fs::metadata(directory).map_err(|error|Failed::new(format!("Reading native account proof folder: {error}")))?;
        if !before.is_file() || before.file_type().is_symlink() || before.nlink()!=1 || before.uid()!=owner.uid() || before.mode()&0o777!=0o600 || before.len()>4096{return Err(Failed::new("The native account proof is not private".into()))}
        let mut file=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK|libc::O_CLOEXEC).open(path).map_err(|error|Failed::new(format!("Opening native account proof: {error}")))?;
        let actual=file.metadata().map_err(|error|Failed::new(format!("Checking native account proof: {error}")))?;
        if before.ino()!=actual.ino() || before.dev()!=actual.dev(){return Err(Failed::new("The native account proof changed while opening".into()))}
        let mut bytes=vec![];file.by_ref().take(4097).read_to_end(&mut bytes).map_err(|error|Failed::new(format!("Reading native account proof: {error}")))?;
        if bytes.len()>4096{return Err(Failed::new("The native account proof is too large".into()))}
        expected.native_auth=Some(serde_json::from_slice(&bytes).map_err(|error|Failed::new(format!("Decoding native account proof: {error}")))?);Ok(identity)
    }
    pub(crate) fn load(directory:&Path)->Result<Option<Self>,Failed>{
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        use std::io::Read;
        let path=directory.join("native-account.json");
        let expected=match std::fs::symlink_metadata(&path){Ok(metadata)=>metadata,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(Failed::new(format!("Reading native account identity: {error}")))};
        let owner=std::fs::metadata(directory).map_err(|error|Failed::new(format!("Reading native account folder: {error}")))?;
        if !expected.is_file() || expected.file_type().is_symlink() || expected.nlink()!=1 || expected.uid()!=owner.uid() || expected.mode()&0o777!=0o600 || expected.len()>4096{return Err(Failed::new("The native account identity is not a private file".into()))}
        let mut file=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path).map_err(|error|Failed::new(format!("Reading native account identity: {error}")))?;
        let actual=file.metadata().map_err(|error|Failed::new(format!("Checking native account identity: {error}")))?;
        if expected.ino()!=actual.ino() || expected.dev()!=actual.dev(){return Err(Failed::new("The native account identity changed while opening it".into()))}
        let mut bytes=vec![];file.by_ref().take(4097).read_to_end(&mut bytes).map_err(|error|Failed::new(format!("Reading native account identity: {error}")))?;
        if bytes.len()>4096{return Err(Failed::new("The native account identity is too large".into()))}
        serde_json::from_slice(&bytes).map(Some).map_err(|error|Failed::new(format!("Decoding native account identity: {error}")))
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    fn status()->AccountStatus{AccountStatus{provider:alondra_core::accounts::Provider::Codex,id:alondra_core::accounts::AccountId::default_account(),name:"Native account".into(),state:alondra_core::accounts::AccountState::SignedIn,email:Some("native@example.test".into()),native_identity:None,plan:None,billing:AccountBilling::Subscription{plan:None,weight:None},sign_ins:0}}
    #[test]
    fn repeated_same_email_reauthentication_preserves_native_identity(){
        let directory=crate::test_support::Directory::new();std::fs::create_dir_all(&directory).unwrap();let mut account=status();
        NativeAccountIdentity::save(&directory,Some(&account)).unwrap();let expected=NativeAccountIdentity::load(&directory).unwrap().unwrap();
        for count in [1,2]{account.sign_ins=count;account.email=Some("NATIVE@example.test".into());account.billing=AccountBilling::Subscription{plan:Some("max".into()),weight:Some(2)};assert!(expected.same_known_email(&account));assert_eq!(expected.change(&account),None);}
        account.email=Some("other@example.test".into());assert!(!expected.same_known_email(&account));assert_eq!(expected.change(&account),Some("identity"));
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

    #[test]
    fn original_nonsecret_account_metadata_survives_retiring_the_host_directory(){
        use std::os::unix::fs::PermissionsExt;use alondra_core::accounts::Provider;
        let directory=crate::test_support::Directory::new();let old=directory.join("old");let next=directory.join("next");std::fs::create_dir_all(&old).unwrap();std::fs::create_dir_all(&next).unwrap();let mut account=status();account.native_identity=Some("workspace-a".into());NativeAccountIdentity::save(&old,Some(&account)).unwrap();let path=old.join("native-auth-account.json");std::fs::write(&path,br#"{"authMethod":"chatgpt","email":"native@example.test","workspaceId":"workspace-a"}"#).unwrap();std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o600)).unwrap();
        let expected=NativeAccountIdentity::load_observed(&old,Provider::Codex).unwrap().unwrap();expected.retain(&next).unwrap();let restored=NativeAccountIdentity::load_observed(&next,Provider::Codex).unwrap().unwrap();assert!(restored.same_known_native_account(Provider::Codex,&account));assert_eq!(std::fs::read(old.join("native-auth-account.json")).unwrap(),std::fs::read(next.join("native-auth-account.json")).unwrap());
    }

}
