CREATE TABLE IF NOT EXISTS lifecycle_tasks(
 task_id TEXT PRIMARY KEY REFERENCES tasks(id),
 epoch TEXT NOT NULL CHECK(length(epoch)>0),
 initial_input TEXT NOT NULL CHECK(length(initial_input)>0),
 revision INTEGER NOT NULL CHECK(revision BETWEEN 0 AND 9007199254740991),
 FOREIGN KEY(task_id,initial_input) REFERENCES lifecycle_inputs(task_id,input_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE IF NOT EXISTS lifecycle_inputs(
 task_id TEXT NOT NULL REFERENCES lifecycle_tasks(task_id), input_id TEXT NOT NULL CHECK(length(input_id)>0),
 body TEXT NOT NULL CHECK(length(body)>0 AND length(CAST(body AS BLOB))<=1048576),
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
 PRIMARY KEY(task_id,generation), UNIQUE(task_id,generation,node_id)
);
CREATE TABLE IF NOT EXISTS lifecycle_current_execution(
 task_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, node_id TEXT NOT NULL UNIQUE,
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
 working_directory TEXT,
 status TEXT NOT NULL CHECK(status IN('prepared','claimed','handed','rejected','uncertain','revoked')),
 claimed_revision INTEGER CHECK(claimed_revision BETWEEN 0 AND 9007199254740991),
 FOREIGN KEY(task_id,generation) REFERENCES lifecycle_executions(task_id,generation),
 FOREIGN KEY(task_id,input_id) REFERENCES lifecycle_inputs(task_id,input_id),
 UNIQUE(task_id,generation,input_id),
 CHECK((kind='launch' AND working_directory IS NOT NULL AND length(working_directory)>0)
    OR (kind='input' AND working_directory IS NULL)),
 CHECK((status IN('prepared','revoked') AND claimed_revision IS NULL)
    OR (status IN('claimed','handed','rejected','uncertain') AND claimed_revision IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS lifecycle_one_launch ON lifecycle_dispatches(task_id,generation) WHERE kind='launch';
CREATE TRIGGER IF NOT EXISTS lifecycle_required_card_stays_actionable BEFORE UPDATE ON interactions
 WHEN EXISTS(SELECT 1 FROM lifecycle_cards c WHERE c.card_id=OLD.id)
 AND (NEW.id<>OLD.id OR NEW.task_id IS NOT OLD.task_id OR NEW.scope_kind<>'task'
   OR NEW.recipient_kind<>'user' OR NEW.kind NOT IN('question','action_request')
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
 BEGIN SELECT RAISE(ABORT,'lifecycle input is immutable'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_execution_immutable BEFORE UPDATE ON lifecycle_executions
 BEGIN SELECT RAISE(ABORT,'lifecycle execution identity is immutable'); END;

CREATE TRIGGER IF NOT EXISTS lifecycle_required_delivery_retained BEFORE UPDATE ON interactions
 WHEN EXISTS(SELECT 1 FROM lifecycle_delivery d WHERE d.card_id=OLD.id)
 AND (NEW.id<>OLD.id OR NEW.task_id IS NOT OLD.task_id OR NEW.scope_kind<>'task' OR NEW.kind<>'delivery')
 BEGIN SELECT RAISE(ABORT,'lifecycle requires its delivery record'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_dispatch_retained BEFORE DELETE ON lifecycle_dispatches
 BEGIN SELECT RAISE(ABORT,'lifecycle dispatch evidence is retained'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_dispatch_transition BEFORE UPDATE ON lifecycle_dispatches
 WHEN NEW.id<>OLD.id OR NEW.task_id<>OLD.task_id OR NEW.generation<>OLD.generation
   OR NEW.input_id<>OLD.input_id OR NEW.kind<>OLD.kind OR NEW.working_directory IS NOT OLD.working_directory
   OR NOT ((OLD.status='prepared' AND NEW.status='claimed' AND NEW.claimed_revision IS NOT NULL)
      OR (OLD.status='prepared' AND NEW.status='revoked' AND NEW.claimed_revision IS NULL)
      OR (OLD.status='claimed' AND NEW.status IN('handed','rejected','uncertain') AND NEW.claimed_revision=OLD.claimed_revision))
 BEGIN SELECT RAISE(ABORT,'lifecycle dispatch transition is invalid'); END;
