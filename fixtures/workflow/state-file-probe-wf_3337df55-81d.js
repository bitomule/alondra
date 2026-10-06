export const meta = {
  name: 'state-file-probe',
  description: 'Tiny 2-phase probe to see whether the workflow state file is written while it runs',
  phases: [{ title: 'First', detail: 'two quick read-only agents' }, { title: 'Second', detail: 'one quick read-only agent' }],
}
phase('First')
const a = await agent('Run `wc -l crates/core/src/model_label.rs` in the current directory and return only the number.', { phase: 'First', model: 'haiku', label: 'count-a' })
const b = await agent('Run `wc -l crates/core/src/views/live.rs` in the current directory and return only the number.', { phase: 'First', model: 'haiku', label: 'count-b' })
phase('Second')
const c = await agent('Run `ls crates/core/src/views | wc -l` in the current directory and return only the number.', { phase: 'Second', model: 'haiku', label: 'count-c' })
return { a, b, c }