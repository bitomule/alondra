CREATE TABLE IF NOT EXISTS lifecycle_tasks(
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),
 epoch TEXT NOT NULL CHECK(length(epoch)>0),
 initial_input TEXT NOT NULL CHECK(length(initial_input)>0),
 revision INTEGER NOT NULL CHECK(revision BETWEEN 0 AND 9007199254740991),
 FOREIGN KEY(task_id,initial_input) REFERENCES lifecycle_inputs(task_id,input_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE IF NOT EXISTS lifecycle_inputs(
 task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id), input_id TEXT NOT NULL CHECK(length(input_id)>0),
 body TEXT CHECK(body IS NULL OR (length(body)>0 AND length(CAST(body AS BLOB))<=1048576)),
 PRIMARY KEY(task_id,input_id)
);
CREATE TABLE IF NOT EXISTS lifecycle_events(
 task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id),
 revision INTEGER NOT NULL CHECK(revision BETWEEN 1 AND 9007199254740991),
 command_id TEXT NOT NULL CHECK(length(command_id)>0),
 event TEXT NOT NULL CHECK(json_valid(event) AND length(CAST(event AS BLOB))<=1048576),
 PRIMARY KEY(task_id,revision), UNIQUE(task_id,command_id)
);
CREATE TABLE IF NOT EXISTS lifecycle_executions(
 task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id), generation INTEGER NOT NULL CHECK(generation BETWEEN 1 AND 9007199254740991),
 node_id TEXT NOT NULL REFERENCES agents(id), instance_id TEXT NOT NULL UNIQUE,
 conversation_id TEXT NOT NULL, host_id TEXT NOT NULL,
 backend TEXT NOT NULL CHECK(backend IN('sdk','claude_bg') AND (backend='claude_bg')=(host_id='claude-bg')),
 PRIMARY KEY(task_id,generation), UNIQUE(task_id,generation,node_id)
);
CREATE TABLE IF NOT EXISTS lifecycle_current_execution(
 task_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, node_id TEXT NOT NULL,
 FOREIGN KEY(task_id,generation,node_id) REFERENCES lifecycle_executions(task_id,generation,node_id)
);
CREATE TABLE IF NOT EXISTS lifecycle_cards(
 task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id), card_id TEXT NOT NULL,
 PRIMARY KEY(task_id,card_id),
 FOREIGN KEY(task_id,card_id) REFERENCES interactions(task_id,id)
);
CREATE TABLE IF NOT EXISTS lifecycle_dependencies(
 task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id), dependency_id TEXT NOT NULL REFERENCES tasks(id),
 PRIMARY KEY(task_id,dependency_id), CHECK(task_id<>dependency_id)
);
CREATE TABLE IF NOT EXISTS lifecycle_delivery(
 task_id TEXT PRIMARY KEY REFERENCES lifecycle_tasks(task_id), card_id TEXT NOT NULL,
 FOREIGN KEY(task_id,card_id) REFERENCES interactions(task_id,id)
);
CREATE TABLE IF NOT EXISTS lifecycle_dispatches(
 id INTEGER PRIMARY KEY,
 task_id TEXT NOT NULL, generation INTEGER NOT NULL, input_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN('launch','input')),
 placement_kind TEXT CHECK(placement_kind IN('local','remote')), placement TEXT,
 status TEXT NOT NULL CHECK(status IN('prepared','claimed','handed','rejected','uncertain','revoked')),
 claimed_revision INTEGER CHECK(claimed_revision BETWEEN 0 AND 9007199254740991),
 FOREIGN KEY(task_id,generation) REFERENCES lifecycle_executions(task_id,generation),
 FOREIGN KEY(task_id,input_id) REFERENCES lifecycle_inputs(task_id,input_id),
 UNIQUE(task_id,generation,input_id), UNIQUE(id,task_id,generation),
 CHECK((kind='launch' AND placement_kind IS NOT NULL AND placement IS NOT NULL AND length(placement)>0)
    OR (kind='input' AND placement_kind IS NULL AND placement IS NULL)),
 CHECK((status IN('prepared','revoked') AND claimed_revision IS NULL)
    OR (status IN('claimed','handed','rejected','uncertain') AND claimed_revision IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS lifecycle_one_launch ON lifecycle_dispatches(task_id,generation) WHERE kind='launch';
CREATE TRIGGER IF NOT EXISTS lifecycle_required_card_stays_actionable BEFORE UPDATE ON interactions
 WHEN EXISTS(SELECT 1 FROM lifecycle_cards c WHERE c.card_id=OLD.id)
 AND (NEW.id<>OLD.id OR NEW.task_id IS NOT OLD.task_id OR NEW.scope_kind<>'task'
   OR NEW.recipient_kind<>OLD.recipient_kind OR NEW.recipient_kind NOT IN('user','orchestrator')
   OR NEW.kind NOT IN('question','action_request')
   OR NEW.answer IS NOT NULL OR NEW.withdrawn<>0 OR json_type(NEW.options) IS NOT 'array'
   OR COALESCE(json_extract(NEW.card,'$.kind') IN('decide','review','authorize','provide','test','do','problem'),0)=0
   OR json_type(NEW.card,'$.need') IS NOT 'text'
   OR COALESCE(length(trim(json_extract(NEW.card,'$.need'),char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288))) BETWEEN 1 AND 90,0)=0
   OR instr(trim(json_extract(NEW.card,'$.need'),char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)),char(10))<>0
   OR instr(json_extract(NEW.card,'$.need'),char(0))<>0)
 BEGIN SELECT RAISE(ABORT,'lifecycle requires an actionable card'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_event_retained BEFORE DELETE ON lifecycle_events
 BEGIN SELECT RAISE(ABORT,'lifecycle journal is retained'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_event_immutable BEFORE UPDATE ON lifecycle_events
 BEGIN SELECT RAISE(ABORT,'lifecycle journal is immutable'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_input_immutable BEFORE UPDATE ON lifecycle_inputs
 WHEN NEW.task_id<>OLD.task_id OR NEW.input_id<>OLD.input_id
   OR NOT ((OLD.body IS NULL AND NEW.body IS NOT NULL)
      OR (OLD.body IS NOT NULL AND NEW.body IS NULL AND EXISTS(SELECT 1 FROM tasks t WHERE t.id=OLD.task_id AND t.state IN('done','cancelled'))))
 BEGIN SELECT RAISE(ABORT,'lifecycle input is immutable'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_execution_immutable BEFORE UPDATE ON lifecycle_executions
 BEGIN SELECT RAISE(ABORT,'lifecycle execution identity is immutable'); END;

CREATE TRIGGER IF NOT EXISTS lifecycle_required_delivery_retained BEFORE UPDATE ON interactions
 WHEN EXISTS(SELECT 1 FROM lifecycle_delivery d WHERE d.card_id=OLD.id)
 AND (NEW.id<>OLD.id OR NEW.task_id IS NOT OLD.task_id OR NEW.scope_kind<>'task' OR NEW.kind<>'delivery')
 BEGIN SELECT RAISE(ABORT,'lifecycle requires its delivery record'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_dispatch_through_sdk BEFORE INSERT ON lifecycle_dispatches
 WHEN EXISTS(SELECT 1 FROM lifecycle_executions e WHERE e.task_id=NEW.task_id AND e.generation=NEW.generation AND e.backend<>'sdk')
   OR EXISTS(SELECT 1 FROM lifecycle_inputs i WHERE i.task_id=NEW.task_id AND i.input_id=NEW.input_id AND i.body IS NULL)
 BEGIN SELECT RAISE(ABORT,'only an SDK execution dispatches, and only an input with its text'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_dispatch_retained BEFORE DELETE ON lifecycle_dispatches
 BEGIN SELECT RAISE(ABORT,'lifecycle dispatch evidence is retained'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_dispatch_transition BEFORE UPDATE ON lifecycle_dispatches
 WHEN NEW.id<>OLD.id OR NEW.task_id<>OLD.task_id OR NEW.generation<>OLD.generation
   OR NEW.input_id<>OLD.input_id OR NEW.kind<>OLD.kind OR NEW.placement_kind IS NOT OLD.placement_kind OR NEW.placement IS NOT OLD.placement
   OR NOT ((OLD.status='prepared' AND NEW.status='claimed' AND NEW.claimed_revision IS NOT NULL)
      OR (OLD.status='prepared' AND NEW.status='revoked' AND NEW.claimed_revision IS NULL)
      OR (OLD.status='claimed' AND NEW.status IN('handed','rejected','uncertain') AND NEW.claimed_revision=OLD.claimed_revision))
 BEGIN SELECT RAISE(ABORT,'lifecycle dispatch transition is invalid'); END;

CREATE TABLE IF NOT EXISTS provider_bindings(
 id TEXT PRIMARY KEY CHECK(length(id)>0),
 driver TEXT NOT NULL CHECK(length(driver)>0), instance TEXT NOT NULL CHECK(length(instance)>0),
 configuration TEXT NOT NULL CHECK(length(configuration)>0), configuration_revision INTEGER NOT NULL CHECK(configuration_revision BETWEEN 1 AND 9007199254740991),
 capability_revision INTEGER NOT NULL CHECK(capability_revision BETWEEN 1 AND 9007199254740991), capabilities INTEGER NOT NULL CHECK(capabilities BETWEEN 0 AND 15),
 credential TEXT NOT NULL CHECK(length(credential)>0), credential_generation INTEGER NOT NULL CHECK(credential_generation BETWEEN 1 AND 9007199254740991),
 UNIQUE(id,driver,instance)
);
CREATE TABLE IF NOT EXISTS provider_active_bindings(
 driver TEXT NOT NULL, instance TEXT NOT NULL, binding_id TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN('active','revoked')),
 PRIMARY KEY(driver,instance),
 FOREIGN KEY(binding_id,driver,instance) REFERENCES provider_bindings(id,driver,instance)
);
CREATE TABLE IF NOT EXISTS provider_conversations(
 task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id), binding_id TEXT NOT NULL REFERENCES provider_bindings(id), conversation_id TEXT NOT NULL CHECK(length(conversation_id)>0),
 PRIMARY KEY(task_id,binding_id,conversation_id)
);
CREATE TABLE IF NOT EXISTS provider_input_metadata(
 task_id TEXT NOT NULL, binding_id TEXT NOT NULL, conversation_id TEXT NOT NULL, input_id TEXT NOT NULL,
 metadata BLOB NOT NULL CHECK(length(metadata) BETWEEN 1 AND 4096),
 PRIMARY KEY(task_id,binding_id,conversation_id,input_id), UNIQUE(task_id,binding_id,conversation_id,metadata),
 FOREIGN KEY(task_id,binding_id,conversation_id) REFERENCES provider_conversations(task_id,binding_id,conversation_id),
 FOREIGN KEY(task_id,input_id) REFERENCES lifecycle_inputs(task_id,input_id)
);
CREATE TRIGGER IF NOT EXISTS provider_input_immutable BEFORE UPDATE ON provider_input_metadata
 BEGIN SELECT RAISE(ABORT,'adapter input binding is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_input_retained BEFORE DELETE ON provider_input_metadata
 BEGIN SELECT RAISE(ABORT,'adapter input binding is retained'); END;
CREATE TABLE IF NOT EXISTS lifecycle_execution_bindings(
 task_id TEXT NOT NULL, generation INTEGER NOT NULL, binding_id TEXT NOT NULL REFERENCES provider_bindings(id), conversation_id TEXT NOT NULL,
 PRIMARY KEY(task_id,generation), UNIQUE(task_id,generation,binding_id),
 FOREIGN KEY(task_id,generation) REFERENCES lifecycle_executions(task_id,generation),
 FOREIGN KEY(task_id,binding_id,conversation_id) REFERENCES provider_conversations(task_id,binding_id,conversation_id)
);
CREATE TRIGGER IF NOT EXISTS provider_binding_immutable BEFORE UPDATE ON provider_bindings
 BEGIN SELECT RAISE(ABORT,'provider binding snapshot is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_binding_retained BEFORE DELETE ON provider_bindings
 BEGIN SELECT RAISE(ABORT,'provider binding snapshot is retained'); END;
CREATE TRIGGER IF NOT EXISTS execution_binding_immutable BEFORE UPDATE ON lifecycle_execution_bindings
 BEGIN SELECT RAISE(ABORT,'execution provider binding is immutable'); END;
CREATE TRIGGER IF NOT EXISTS execution_binding_retained BEFORE DELETE ON lifecycle_execution_bindings
 BEGIN SELECT RAISE(ABORT,'execution provider binding is retained'); END;

CREATE TABLE IF NOT EXISTS provider_conversation_metadata(
 task_id TEXT NOT NULL, binding_id TEXT NOT NULL, conversation_id TEXT NOT NULL, metadata BLOB NOT NULL CHECK(length(metadata) BETWEEN 1 AND 4096),
 PRIMARY KEY(task_id,binding_id,conversation_id), UNIQUE(binding_id,metadata),
 FOREIGN KEY(task_id,binding_id,conversation_id) REFERENCES provider_conversations(task_id,binding_id,conversation_id)
);
CREATE TRIGGER IF NOT EXISTS provider_conversation_metadata_immutable BEFORE UPDATE ON provider_conversation_metadata
 BEGIN SELECT RAISE(ABORT,'adapter conversation binding is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_conversation_metadata_retained BEFORE DELETE ON provider_conversation_metadata
 BEGIN SELECT RAISE(ABORT,'adapter conversation binding is retained'); END;
CREATE TABLE IF NOT EXISTS provider_commands(
 intent_id INTEGER PRIMARY KEY,task_id TEXT NOT NULL,generation INTEGER NOT NULL,binding_id TEXT NOT NULL,
 digest BLOB NOT NULL CHECK(length(digest)=32),bytes BLOB NOT NULL CHECK(length(bytes) BETWEEN 1 AND 1048576),
 FOREIGN KEY(intent_id,task_id,generation) REFERENCES lifecycle_dispatches(id,task_id,generation),
 FOREIGN KEY(task_id,generation,binding_id) REFERENCES lifecycle_execution_bindings(task_id,generation,binding_id)
);
CREATE TRIGGER IF NOT EXISTS provider_command_immutable BEFORE UPDATE ON provider_commands
 BEGIN SELECT RAISE(ABORT,'provider command evidence is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_command_retained BEFORE DELETE ON provider_commands
 WHEN EXISTS(SELECT 1 FROM lifecycle_current_execution c WHERE c.task_id=OLD.task_id AND c.generation=OLD.generation)
   OR EXISTS(SELECT 1 FROM lifecycle_dispatches d WHERE d.id=OLD.intent_id AND d.status='claimed')
 BEGIN SELECT RAISE(ABORT,'provider command evidence is retained'); END;
CREATE TABLE IF NOT EXISTS provider_observations(
 task_id TEXT NOT NULL,generation INTEGER NOT NULL,binding_id TEXT NOT NULL,cursor INTEGER NOT NULL CHECK(cursor BETWEEN 1 AND 9007199254740991),
 digest BLOB NOT NULL CHECK(length(digest)=32),bytes BLOB NOT NULL CHECK(length(bytes) BETWEEN 1 AND 1048576),
 PRIMARY KEY(task_id,generation,cursor),
 FOREIGN KEY(task_id,generation,binding_id) REFERENCES lifecycle_execution_bindings(task_id,generation,binding_id)
);
CREATE TABLE IF NOT EXISTS provider_observation_heads(
 task_id TEXT NOT NULL,generation INTEGER NOT NULL,cursor INTEGER NOT NULL,
 PRIMARY KEY(task_id,generation),
 FOREIGN KEY(task_id,generation,cursor) REFERENCES provider_observations(task_id,generation,cursor)
);
CREATE TRIGGER IF NOT EXISTS provider_observation_immutable BEFORE UPDATE ON provider_observations
 WHEN NEW.task_id IS NOT OLD.task_id OR NEW.generation IS NOT OLD.generation OR NEW.binding_id IS NOT OLD.binding_id OR NEW.cursor IS NOT OLD.cursor OR NEW.digest IS NOT OLD.digest OR NEW.bytes IS NOT X'00'
  OR NOT EXISTS(SELECT 1 FROM provider_normalization_checkpoints c WHERE c.task_id=OLD.task_id AND c.generation=OLD.generation AND c.cursor>OLD.cursor)
 BEGIN SELECT RAISE(ABORT,'provider observation evidence is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_observation_retained BEFORE DELETE ON provider_observations
 WHEN OLD.bytes IS NOT X'00'
 BEGIN SELECT RAISE(ABORT,'provider observation evidence is retained'); END;

CREATE TABLE IF NOT EXISTS provider_execution_owners(
 task_id TEXT NOT NULL,
 generation INTEGER NOT NULL,
 binding_id TEXT NOT NULL,
 owner_generation INTEGER NOT NULL CHECK(owner_generation>0 AND owner_generation<=9007199254740991),
 PRIMARY KEY(task_id,generation),
 FOREIGN KEY(task_id,generation,binding_id) REFERENCES lifecycle_execution_bindings(task_id,generation,binding_id)
);
CREATE TRIGGER IF NOT EXISTS execution_owner_monotonic BEFORE UPDATE ON provider_execution_owners
 WHEN NEW.task_id IS NOT OLD.task_id OR NEW.generation IS NOT OLD.generation OR NEW.binding_id IS NOT OLD.binding_id OR NEW.owner_generation<=OLD.owner_generation
 BEGIN SELECT RAISE(ABORT,'execution ownership is strictly monotonic'); END;
CREATE TRIGGER IF NOT EXISTS execution_owner_retained BEFORE DELETE ON provider_execution_owners
 BEGIN SELECT RAISE(ABORT,'execution owner fence is retained'); END;

CREATE TABLE IF NOT EXISTS provider_normalization_checkpoints(
 task_id TEXT NOT NULL,
 generation INTEGER NOT NULL,
 binding_id TEXT NOT NULL,
 cursor INTEGER NOT NULL CHECK(cursor>0 AND cursor<=9007199254740991),
 digest BLOB NOT NULL CHECK(length(digest)=32),
 bytes BLOB NOT NULL CHECK(length(bytes) BETWEEN 1 AND 65536),
 PRIMARY KEY(task_id,generation),
 FOREIGN KEY(task_id,generation,binding_id) REFERENCES lifecycle_execution_bindings(task_id,generation,binding_id),
 FOREIGN KEY(task_id,generation,cursor) REFERENCES provider_observations(task_id,generation,cursor)
);
CREATE TRIGGER IF NOT EXISTS normalization_checkpoint_monotonic BEFORE UPDATE ON provider_normalization_checkpoints
 WHEN NEW.task_id IS NOT OLD.task_id OR NEW.generation IS NOT OLD.generation OR NEW.binding_id IS NOT OLD.binding_id OR NEW.cursor<=OLD.cursor
 BEGIN SELECT RAISE(ABORT,'normalization checkpoint identity and cursor are fenced'); END;
CREATE TRIGGER IF NOT EXISTS normalization_checkpoint_retained BEFORE DELETE ON provider_normalization_checkpoints
 BEGIN SELECT RAISE(ABORT,'normalization checkpoint is retained'); END;
