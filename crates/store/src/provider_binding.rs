use crate::{Error, Result, read_number, sql_number};
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::{Capability, ProviderBinding};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeSet;

const CAPABILITIES: [Capability; 4] = [
    Capability::OfferInput,
    Capability::Interrupt,
    Capability::BackgroundWork,
    Capability::DurableResume,
];
/// Capabilities the snapshot's own column has no bits for: kept beside it, only when one is held.
const CONTROLS: [Capability; 5] = [
    Capability::SetModel,
    Capability::SetPermissionMode,
    Capability::Compact,
    Capability::McpStatus,
    Capability::Decisions,
];
fn mask(binding: &ProviderBinding, capabilities: &[Capability]) -> i64 {
    capabilities
        .iter()
        .enumerate()
        .filter(|(_, cap)| binding.permits(**cap))
        .map(|(i, _)| 1_i64 << i)
        .sum()
}
pub(super) fn register(connection: &Connection, binding: &ProviderBinding) -> Result<()> {
    let inserted = connection.execute("INSERT INTO provider_bindings(id,driver,instance,configuration,configuration_revision,capability_revision,capabilities,credential,credential_generation) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(id) DO NOTHING",
        params![binding.id(),binding.driver(),binding.instance(),binding.configuration(),sql_number(binding.configuration_revision())?,sql_number(binding.capability_revision())?,mask(binding, &CAPABILITIES),binding.credential(),sql_number(binding.credential_generation())?])?;
    let controls = mask(binding, &CONTROLS);
    if inserted == 1 && controls != 0 {
        connection.execute("INSERT INTO provider_binding_controls(binding_id,capabilities) VALUES(?1,?2)", params![binding.id(), controls])?;
    }
    if load(connection, binding.id())? != *binding {
        return Err(Error::BindingConflict);
    }
    Ok(())
}
pub(super) fn activate_initial(connection: &Connection, binding: &ProviderBinding) -> Result<()> {
    register(connection, binding)?;
    connection.execute("INSERT INTO provider_active_bindings(driver,instance,binding_id,status) VALUES(?1,?2,?3,'active') ON CONFLICT(driver,instance) DO NOTHING",params![binding.driver(),binding.instance(),binding.id()])?;
    require_active(connection, binding)
}
pub(super) fn revoke(connection: &Connection, binding: &ProviderBinding) -> Result<()> {
    let changed=connection.execute("UPDATE provider_active_bindings SET status='revoked' WHERE driver=?1 AND instance=?2 AND binding_id=?3 AND status='active'",params![binding.driver(),binding.instance(),binding.id()])?;
    if changed != 1 {
        return Err(Error::BindingRevoked);
    }
    Ok(())
}
pub(super) fn bind(
    connection: &Connection,
    stamp: &ExecutionStamp,
    binding: &ProviderBinding,
) -> Result<()> {
    register(connection, binding)?;
    require_active(connection, binding)?;
    connection.execute("INSERT INTO provider_conversations(task_id,binding_id,conversation_id) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",params![stamp.task().0,binding.id(),stamp.conversation().as_str()])?;
    connection.execute("INSERT INTO lifecycle_execution_bindings(task_id,generation,binding_id,conversation_id) VALUES(?1,?2,?3,?4) ON CONFLICT(task_id,generation) DO NOTHING",params![stamp.task().0,sql_number(stamp.generation())?,binding.id(),stamp.conversation().as_str()])?;
    if for_execution(connection, stamp)? != *binding {
        return Err(Error::BindingConflict);
    }
    Ok(())
}
pub(super) fn for_execution(
    connection: &Connection,
    stamp: &ExecutionStamp,
) -> Result<ProviderBinding> {
    let id: Option<String> = connection.query_row("SELECT binding_id FROM lifecycle_execution_bindings WHERE task_id=?1 AND generation=?2 AND conversation_id=?3",params![stamp.task().0,sql_number(stamp.generation())?,stamp.conversation().as_str()],|row|row.get(0)).optional()?;
    load(connection, &id.ok_or(Error::MissingBinding)?)
}
pub(super) fn require_active(connection: &Connection, binding: &ProviderBinding) -> Result<()> {
    let active: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM provider_active_bindings WHERE driver=?1 AND instance=?2 AND binding_id=?3 AND status='active')",params![binding.driver(),binding.instance(),binding.id()],|row|row.get(0))?;
    if !active {
        return Err(Error::BindingRevoked);
    }
    Ok(())
}
fn load(connection: &Connection, id: &str) -> Result<ProviderBinding> {
    let row=connection.query_row("SELECT driver,instance,configuration,configuration_revision,capability_revision,capabilities,credential,credential_generation FROM provider_bindings WHERE id=?1",[id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,read_number(row,3)?,read_number(row,4)?,row.get::<_,i64>(5)?,row.get::<_,String>(6)?,read_number(row,7)?))).optional()?.ok_or(Error::MissingBinding)?;
    let controls: i64 = connection.query_row("SELECT capabilities FROM provider_binding_controls WHERE binding_id=?1", [id], |row| row.get(0)).optional()?.unwrap_or(0);
    if !(0..=15).contains(&row.5) || !(0..=31).contains(&controls) {
        return Err(Error::BindingConflict);
    }
    let held = |capabilities: &'static [Capability], mask: i64| {
        capabilities.iter().enumerate().filter(move |(i, _)| mask & (1_i64 << i) != 0).map(|(_, cap)| *cap)
    };
    let capabilities: BTreeSet<_> = held(&CAPABILITIES, row.5).chain(held(&CONTROLS, controls)).collect();
    ProviderBinding::new(
        id.into(),
        row.0,
        row.1,
        row.2,
        row.3,
        row.4,
        capabilities,
        row.6,
        row.7,
    )
    .map_err(|_| Error::BindingConflict)
}
