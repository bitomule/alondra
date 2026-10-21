CREATE TABLE IF NOT EXISTS provider_binding_controls(
 binding_id TEXT PRIMARY KEY REFERENCES provider_bindings(id),
 capabilities INTEGER NOT NULL CHECK(capabilities BETWEEN 1 AND 511)
);
CREATE TRIGGER IF NOT EXISTS provider_binding_controls_immutable BEFORE UPDATE ON provider_binding_controls
 BEGIN SELECT RAISE(ABORT,'provider binding snapshot is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_binding_controls_retained BEFORE DELETE ON provider_binding_controls
 BEGIN SELECT RAISE(ABORT,'provider binding snapshot is retained'); END;
CREATE TABLE IF NOT EXISTS lifecycle_decision_requests(
 task_id TEXT NOT NULL, generation INTEGER NOT NULL,
 epoch INTEGER NOT NULL CHECK(epoch BETWEEN 0 AND 9007199254740991),
 request_id TEXT NOT NULL CHECK(length(request_id) BETWEEN 1 AND 256),
 tool_use_id TEXT CHECK(tool_use_id IS NULL OR length(tool_use_id) BETWEEN 1 AND 256),
 kind TEXT NOT NULL CHECK(kind IN('permission','question','plan','elicitation')),
 payload BLOB NOT NULL CHECK(length(payload) BETWEEN 1 AND 1048576),
 PRIMARY KEY(task_id,generation,epoch),
 FOREIGN KEY(task_id,generation) REFERENCES lifecycle_executions(task_id,generation),
 CHECK((kind='elicitation')=(tool_use_id IS NULL))
);
CREATE TRIGGER IF NOT EXISTS lifecycle_decision_request_through_sdk BEFORE INSERT ON lifecycle_decision_requests
 WHEN NOT EXISTS(SELECT 1 FROM lifecycle_executions e WHERE e.task_id=NEW.task_id AND e.generation=NEW.generation AND e.backend='sdk')
 BEGIN SELECT RAISE(ABORT,'only an SDK execution asks for decisions'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_decision_request_immutable BEFORE UPDATE ON lifecycle_decision_requests
 BEGIN SELECT RAISE(ABORT,'lifecycle decision request evidence is immutable'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_decision_request_retained BEFORE DELETE ON lifecycle_decision_requests
 BEGIN SELECT RAISE(ABORT,'lifecycle decision request evidence is retained'); END;
CREATE TABLE IF NOT EXISTS lifecycle_controls(
 id INTEGER PRIMARY KEY,
 task_id TEXT NOT NULL, generation INTEGER NOT NULL, control_id TEXT NOT NULL CHECK(length(control_id)>0),
 kind TEXT NOT NULL CHECK(kind IN('interrupt','force_stop','set_model','set_permission_mode','mcp_status','mcp_reconnect','mcp_sign_in','mcp_clear_auth','respond')),
 argument TEXT,
 epoch INTEGER,
 status TEXT NOT NULL CHECK(status IN('prepared','claimed','handed','rejected','uncertain','revoked')),
 claimed_revision INTEGER CHECK(claimed_revision BETWEEN 0 AND 9007199254740991),
 FOREIGN KEY(task_id,generation) REFERENCES lifecycle_executions(task_id,generation),
 FOREIGN KEY(task_id,generation,epoch) REFERENCES lifecycle_decision_requests(task_id,generation,epoch),
 UNIQUE(task_id,control_id), UNIQUE(id,task_id,generation),
 CHECK((kind='respond')=(epoch IS NOT NULL)),
 CHECK((kind='interrupt' AND (argument IS NULL OR length(argument)>0))
    OR (kind='force_stop' AND argument IS NOT NULL AND length(argument)>0)
    OR (kind='set_model' AND argument IS NOT NULL AND length(CAST(argument AS BLOB)) BETWEEN 1 AND 128)
    OR (kind='set_permission_mode' AND argument IS NOT NULL AND argument IN('default','plan','dont_ask'))
    OR (kind='mcp_status' AND argument IS NULL)
    OR (kind IN('mcp_reconnect','mcp_sign_in','mcp_clear_auth') AND argument IS NOT NULL AND length(CAST(argument AS BLOB)) BETWEEN 1 AND 256)
    OR (kind='respond' AND argument IS NOT NULL AND json_valid(argument) AND length(CAST(argument AS BLOB)) BETWEEN 1 AND 1048576)),
 CHECK((status IN('prepared','revoked') AND claimed_revision IS NULL)
    OR (status IN('claimed','handed','rejected','uncertain') AND claimed_revision IS NOT NULL))
);
CREATE TRIGGER IF NOT EXISTS lifecycle_control_through_sdk BEFORE INSERT ON lifecycle_controls
 WHEN NOT EXISTS(SELECT 1 FROM lifecycle_executions e WHERE e.task_id=NEW.task_id AND e.generation=NEW.generation AND e.backend='sdk')
 BEGIN SELECT RAISE(ABORT,'only an SDK execution is controlled'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_control_retained BEFORE DELETE ON lifecycle_controls
 BEGIN SELECT RAISE(ABORT,'lifecycle control evidence is retained'); END;
CREATE TRIGGER IF NOT EXISTS lifecycle_control_transition BEFORE UPDATE ON lifecycle_controls
 WHEN NEW.id<>OLD.id OR NEW.task_id<>OLD.task_id OR NEW.generation<>OLD.generation OR NEW.control_id<>OLD.control_id
   OR NEW.kind<>OLD.kind OR NEW.argument IS NOT OLD.argument OR NEW.epoch IS NOT OLD.epoch
   OR NOT ((OLD.status='prepared' AND NEW.status='claimed' AND NEW.claimed_revision IS NOT NULL)
      OR (OLD.status='prepared' AND NEW.status='revoked' AND NEW.claimed_revision IS NULL)
      OR (OLD.status='claimed' AND NEW.status IN('handed','rejected','uncertain') AND NEW.claimed_revision=OLD.claimed_revision))
 BEGIN SELECT RAISE(ABORT,'lifecycle control transition is invalid'); END;
CREATE TABLE IF NOT EXISTS provider_control_commands(
 control_id INTEGER PRIMARY KEY,task_id TEXT NOT NULL,generation INTEGER NOT NULL,binding_id TEXT NOT NULL,
 digest BLOB NOT NULL CHECK(length(digest)=32),bytes BLOB NOT NULL CHECK(length(bytes) BETWEEN 1 AND 1048576),
 FOREIGN KEY(control_id,task_id,generation) REFERENCES lifecycle_controls(id,task_id,generation),
 FOREIGN KEY(task_id,generation,binding_id) REFERENCES lifecycle_execution_bindings(task_id,generation,binding_id)
);
CREATE TRIGGER IF NOT EXISTS provider_control_command_immutable BEFORE UPDATE ON provider_control_commands
 BEGIN SELECT RAISE(ABORT,'provider control command evidence is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_control_command_retained BEFORE DELETE ON provider_control_commands
 BEGIN SELECT RAISE(ABORT,'provider control command evidence is retained'); END;
