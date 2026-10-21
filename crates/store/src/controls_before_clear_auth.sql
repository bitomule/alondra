CREATE TABLE IF NOT EXISTS provider_binding_controls(
 binding_id TEXT PRIMARY KEY REFERENCES provider_bindings(id),
 capabilities INTEGER NOT NULL CHECK(capabilities BETWEEN 1 AND 255)
);
CREATE TABLE IF NOT EXISTS lifecycle_controls(
 id INTEGER PRIMARY KEY,
 task_id TEXT NOT NULL, generation INTEGER NOT NULL, control_id TEXT NOT NULL CHECK(length(control_id)>0),
 kind TEXT NOT NULL CHECK(kind IN('interrupt','force_stop','set_model','set_permission_mode','mcp_status','mcp_reconnect','mcp_sign_in','respond')),
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
    OR (kind IN('mcp_reconnect','mcp_sign_in') AND argument IS NOT NULL AND length(CAST(argument AS BLOB)) BETWEEN 1 AND 256)
    OR (kind='respond' AND argument IS NOT NULL AND json_valid(argument) AND length(CAST(argument AS BLOB)) BETWEEN 1 AND 1048576)),
 CHECK((status IN('prepared','revoked') AND claimed_revision IS NULL)
    OR (status IN('claimed','handed','rejected','uncertain') AND claimed_revision IS NOT NULL))
);
