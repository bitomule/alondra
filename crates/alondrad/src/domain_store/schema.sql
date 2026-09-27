CREATE TABLE domain_meta (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), data_epoch TEXT NOT NULL CHECK(length(data_epoch)>0),
 revision INTEGER NOT NULL CHECK(revision BETWEEN 0 AND 9007199254740991),
 orchestrator_generation INTEGER NOT NULL CHECK(orchestrator_generation>=0)
);
CREATE TABLE requests (id TEXT PRIMARY KEY CHECK(length(id)>0), body TEXT NOT NULL, closed INTEGER NOT NULL CHECK(closed IN(0,1)));
CREATE TABLE agents (id TEXT PRIMARY KEY CHECK(length(id)>0), name TEXT NOT NULL, connected INTEGER NOT NULL CHECK(connected IN(0,1)));
CREATE TABLE tasks (
 id TEXT PRIMARY KEY CHECK(length(id)>0), request_id TEXT NOT NULL REFERENCES requests(id), agent_id TEXT REFERENCES agents(id), title TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN('not_started','running','needs_you','in_review','waiting_dependency','closing','done','cancelled')),
 revision INTEGER NOT NULL CHECK(revision BETWEEN 0 AND 9007199254740991), started INTEGER NOT NULL CHECK(started IN(0,1)), review_prs INTEGER NOT NULL CHECK(review_prs IN(0,1)),
 final_presentation TEXT, pr_id TEXT, archived INTEGER NOT NULL CHECK(archived IN(0,1)),
 FOREIGN KEY(id,final_presentation) REFERENCES interactions(task_id,id) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(id,pr_id) REFERENCES pull_requests(task_id,id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE task_dependencies(task_id TEXT NOT NULL REFERENCES tasks(id), depends_on_id TEXT NOT NULL REFERENCES tasks(id), PRIMARY KEY(task_id,depends_on_id), CHECK(task_id<>depends_on_id));
CREATE TABLE documents(id TEXT PRIMARY KEY CHECK(length(id)>0),title TEXT NOT NULL,current_revision TEXT NOT NULL,
 FOREIGN KEY(id,current_revision) REFERENCES document_revisions(document_id,id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE document_revisions(id TEXT PRIMARY KEY CHECK(length(id)>0), document_id TEXT NOT NULL REFERENCES documents(id) DEFERRABLE INITIALLY DEFERRED,number INTEGER NOT NULL CHECK(number BETWEEN 0 AND 9007199254740991),content_digest TEXT NOT NULL,content_reference TEXT NOT NULL,UNIQUE(document_id,number),UNIQUE(document_id,id));
CREATE TRIGGER immutable_document_revision_update BEFORE UPDATE ON document_revisions WHEN NEW.id<>OLD.id OR NEW.document_id<>OLD.document_id OR NEW.number<>OLD.number OR NEW.content_digest<>OLD.content_digest OR NEW.content_reference<>OLD.content_reference BEGIN SELECT RAISE(ABORT,'document revision is immutable'); END;
CREATE TRIGGER immutable_document_revision_delete BEFORE DELETE ON document_revisions BEGIN SELECT RAISE(ABORT,'document revision is retained'); END;
CREATE TABLE task_documents(task_id TEXT NOT NULL REFERENCES tasks(id),document_id TEXT NOT NULL REFERENCES documents(id),role TEXT NOT NULL,PRIMARY KEY(task_id,document_id));
CREATE TABLE pull_requests(id TEXT PRIMARY KEY CHECK(length(id)>0),task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id),host TEXT NOT NULL,repository TEXT NOT NULL,number INTEGER NOT NULL CHECK(number>0),head TEXT NOT NULL,base TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('open','closed','merged')),merged_head TEXT,allowed_methods TEXT NOT NULL CHECK(json_valid(allowed_methods)),UNIQUE(host,repository,number),UNIQUE(task_id,id));
CREATE TRIGGER immutable_pr_ownership BEFORE UPDATE ON pull_requests WHEN NEW.id<>OLD.id OR NEW.task_id<>OLD.task_id OR NEW.host<>OLD.host OR NEW.repository<>OLD.repository OR NEW.number<>OLD.number BEGIN SELECT RAISE(ABORT,'PR ownership is immutable'); END;
CREATE TRIGGER retained_pr BEFORE DELETE ON pull_requests BEGIN SELECT RAISE(ABORT,'PR ownership is retained for Task lifetime'); END;
CREATE TABLE pull_request_reviews(pr_id TEXT PRIMARY KEY REFERENCES pull_requests(id));
CREATE TABLE interactions (
 id TEXT PRIMARY KEY CHECK(length(id)>0),scope_kind TEXT NOT NULL CHECK(scope_kind IN('task','orchestrator')),task_id TEXT REFERENCES tasks(id),request_id TEXT REFERENCES requests(id),
 author_kind TEXT NOT NULL CHECK(author_kind IN('agent','orchestrator','system')),author_agent_id TEXT REFERENCES agents(id),author_generation INTEGER,
 recipient_kind TEXT NOT NULL CHECK(recipient_kind IN('user','agent','orchestrator')),recipient_agent_id TEXT REFERENCES agents(id),
 body TEXT NOT NULL,blocking INTEGER NOT NULL CHECK(blocking IN(0,1)),kind TEXT NOT NULL CHECK(kind IN('information','question','action_request','delivery')),
 revision INTEGER NOT NULL CHECK(revision BETWEEN 0 AND 9007199254740991),ordering INTEGER NOT NULL CHECK(ordering BETWEEN 0 AND 9007199254740991),
 options TEXT CHECK(options IS NULL OR json_valid(options)),answer TEXT CHECK(answer IS NULL OR json_valid(answer)),is_final INTEGER CHECK(is_final IN(0,1)),
 acceptance_user TEXT,acceptance_revision INTEGER,acceptance_valid INTEGER CHECK(acceptance_valid IN(0,1)),
 UNIQUE(task_id,id),
 CHECK((scope_kind='task' AND task_id IS NOT NULL AND request_id IS NULL) OR (scope_kind='orchestrator' AND task_id IS NULL)),
 CHECK((author_kind='agent' AND author_agent_id IS NOT NULL AND author_generation IS NULL) OR (author_kind='orchestrator' AND author_agent_id IS NULL AND author_generation IS NOT NULL) OR (author_kind='system' AND author_agent_id IS NULL AND author_generation IS NULL)),
 CHECK((recipient_kind='agent' AND recipient_agent_id IS NOT NULL) OR (recipient_kind<>'agent' AND recipient_agent_id IS NULL)),
 CHECK((kind='information' AND blocking=0 AND options IS NULL AND answer IS NULL AND is_final IS NULL) OR (kind IN('question','action_request') AND options IS NOT NULL AND is_final IS NULL) OR (kind='delivery' AND scope_kind='task' AND options IS NULL AND answer IS NULL AND is_final IS NOT NULL)),
 CHECK((acceptance_user IS NULL AND acceptance_revision IS NULL AND acceptance_valid IS NULL) OR (kind='delivery' AND acceptance_user IS NOT NULL AND acceptance_revision IS NOT NULL AND acceptance_valid IS NOT NULL))
);
CREATE TABLE interaction_reads(interaction_id TEXT NOT NULL REFERENCES interactions(id),user_id TEXT NOT NULL,PRIMARY KEY(interaction_id,user_id));
CREATE TABLE interaction_subjects(interaction_id TEXT PRIMARY KEY,task_id TEXT NOT NULL,pr_id TEXT,document_id TEXT,revision_id TEXT,
 FOREIGN KEY(task_id,interaction_id) REFERENCES interactions(task_id,id),FOREIGN KEY(task_id,pr_id) REFERENCES pull_requests(task_id,id),
 FOREIGN KEY(task_id,document_id) REFERENCES task_documents(task_id,document_id),FOREIGN KEY(document_id,revision_id) REFERENCES document_revisions(document_id,id),
 CHECK((pr_id IS NOT NULL AND document_id IS NULL AND revision_id IS NULL) OR (pr_id IS NULL AND document_id IS NOT NULL AND revision_id IS NOT NULL)));
CREATE TABLE delivery_targets(interaction_id TEXT NOT NULL,ordinal INTEGER NOT NULL,task_id TEXT NOT NULL,kind TEXT NOT NULL CHECK(kind IN('text','document_revision','pull_request')),text TEXT,document_id TEXT,revision_id TEXT,pr_id TEXT,head TEXT,
 PRIMARY KEY(interaction_id,ordinal),FOREIGN KEY(task_id,interaction_id) REFERENCES interactions(task_id,id),FOREIGN KEY(task_id,document_id) REFERENCES task_documents(task_id,document_id),FOREIGN KEY(document_id,revision_id) REFERENCES document_revisions(document_id,id),FOREIGN KEY(task_id,pr_id) REFERENCES pull_requests(task_id,id),
 CHECK((kind='text' AND text IS NOT NULL AND document_id IS NULL AND revision_id IS NULL AND pr_id IS NULL AND head IS NULL) OR (kind='document_revision' AND text IS NULL AND document_id IS NOT NULL AND revision_id IS NOT NULL AND pr_id IS NULL AND head IS NULL) OR (kind='pull_request' AND text IS NULL AND document_id IS NULL AND revision_id IS NULL AND pr_id IS NOT NULL AND head IS NOT NULL)));
CREATE TABLE delivery_feedback(interaction_id TEXT NOT NULL REFERENCES interactions(id),ordinal INTEGER NOT NULL,user_id TEXT NOT NULL,body TEXT NOT NULL,at_revision INTEGER NOT NULL,PRIMARY KEY(interaction_id,ordinal));
CREATE TABLE document_reviews(task_id TEXT NOT NULL,document_id TEXT NOT NULL,PRIMARY KEY(task_id,document_id),FOREIGN KEY(task_id,document_id) REFERENCES task_documents(task_id,document_id));
CREATE TABLE document_decisions(task_id TEXT NOT NULL,document_id TEXT NOT NULL,ordinal INTEGER NOT NULL,revision_id TEXT NOT NULL,presentation_id TEXT,accepted INTEGER NOT NULL CHECK(accepted IN(0,1)),user_id TEXT NOT NULL,at_revision INTEGER NOT NULL,
 PRIMARY KEY(task_id,document_id,ordinal),FOREIGN KEY(task_id,document_id) REFERENCES document_reviews(task_id,document_id),FOREIGN KEY(document_id,revision_id) REFERENCES document_revisions(document_id,id),FOREIGN KEY(task_id,presentation_id) REFERENCES interactions(task_id,id));
CREATE TABLE pr_decisions(pr_id TEXT NOT NULL REFERENCES pull_request_reviews(pr_id),ordinal INTEGER NOT NULL,task_id TEXT NOT NULL,presentation_id TEXT NOT NULL,head TEXT NOT NULL,accepted INTEGER NOT NULL CHECK(accepted IN(0,1)),user_id TEXT NOT NULL,at_revision INTEGER NOT NULL,
 PRIMARY KEY(pr_id,ordinal),FOREIGN KEY(task_id,pr_id) REFERENCES pull_requests(task_id,id),FOREIGN KEY(task_id,presentation_id) REFERENCES interactions(task_id,id));
CREATE TABLE merge_authorizations(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,pr_id TEXT NOT NULL,presentation_id TEXT NOT NULL,head TEXT NOT NULL,method TEXT NOT NULL CHECK(method IN('merge','squash','rebase')),user_id TEXT NOT NULL,valid INTEGER NOT NULL CHECK(valid IN(0,1)),attempt TEXT NOT NULL CHECK(attempt IN('unclaimed','claimed','uncertain','confirmed','failed')),claimed_by TEXT REFERENCES agents(id),at_revision INTEGER NOT NULL,
 FOREIGN KEY(task_id,pr_id) REFERENCES pull_requests(task_id,id),FOREIGN KEY(task_id,presentation_id) REFERENCES interactions(task_id,id));
CREATE TABLE task_stacks(task_id TEXT PRIMARY KEY REFERENCES tasks(id),upstream_task TEXT NOT NULL,presentation_id TEXT NOT NULL,pr_id TEXT NOT NULL,head TEXT NOT NULL,CHECK(task_id<>upstream_task),FOREIGN KEY(upstream_task,presentation_id) REFERENCES interactions(task_id,id),FOREIGN KEY(upstream_task,pr_id) REFERENCES pull_requests(task_id,id));
CREATE TABLE stack_readiness(task_id TEXT PRIMARY KEY,presentation_id TEXT NOT NULL,pr_id TEXT NOT NULL,head TEXT NOT NULL,ready INTEGER NOT NULL CHECK(ready IN(0,1)),FOREIGN KEY(task_id,presentation_id) REFERENCES interactions(task_id,id),FOREIGN KEY(task_id,pr_id) REFERENCES pull_requests(task_id,id));
CREATE TABLE semantic_receipts(actor TEXT NOT NULL,command_id TEXT NOT NULL,data_epoch TEXT NOT NULL,digest BLOB NOT NULL CHECK(length(digest)=32),result TEXT NOT NULL CHECK(json_valid(result)),PRIMARY KEY(actor,command_id,data_epoch));
CREATE TABLE local_command_results(sender TEXT NOT NULL,request_id TEXT NOT NULL,digest BLOB NOT NULL CHECK(length(digest)=32),result TEXT NOT NULL CHECK(json_valid(result)),PRIMARY KEY(sender,request_id));
CREATE TABLE command_audit(revision INTEGER PRIMARY KEY,actor TEXT NOT NULL,command_id TEXT NOT NULL,data_epoch TEXT NOT NULL,command TEXT NOT NULL CHECK(json_valid(command)),FOREIGN KEY(actor,command_id,data_epoch) REFERENCES semantic_receipts(actor,command_id,data_epoch) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE domain_messages(id INTEGER PRIMARY KEY,revision INTEGER NOT NULL REFERENCES command_audit(revision),ordinal INTEGER NOT NULL,recipient_kind TEXT NOT NULL CHECK(recipient_kind IN('agent','orchestrator')),agent_id TEXT REFERENCES agents(id),task_id TEXT REFERENCES tasks(id),interaction_id TEXT REFERENCES interactions(id),message TEXT NOT NULL,UNIQUE(revision,ordinal),CHECK((recipient_kind='agent' AND agent_id IS NOT NULL) OR (recipient_kind='orchestrator' AND agent_id IS NULL)));
CREATE TABLE domain_message_user_targets(message_id INTEGER PRIMARY KEY REFERENCES domain_messages(id));
CREATE TABLE delivery_attempts(id INTEGER PRIMARY KEY,message_id INTEGER NOT NULL REFERENCES domain_messages(id),state TEXT NOT NULL CHECK(state IN('dispatching','uncertain','handed','retryable','discarded')),version INTEGER NOT NULL CHECK(version BETWEEN 1 AND 9007199254740991),reason TEXT);
CREATE TABLE domain_outbox(message_id INTEGER PRIMARY KEY REFERENCES domain_messages(id),state TEXT NOT NULL CHECK(state IN('pending','dispatching','uncertain','handed','retryable','discarded')));
-- A legacy Store::open fails while creating its events index, before replay/writes.
CREATE VIEW events AS SELECT revision AS seq,0 AS at,'domain_v2' AS kind,NULL AS session,command AS body FROM command_audit;
CREATE TABLE interaction_attachments(
 interaction_id TEXT NOT NULL REFERENCES interactions(id),kind TEXT NOT NULL CHECK(kind IN('interaction','answer','feedback')),
 feedback_ordinal INTEGER,ordinal INTEGER NOT NULL CHECK(ordinal>=0),id TEXT NOT NULL,name TEXT NOT NULL,content_digest TEXT NOT NULL,content_reference TEXT NOT NULL,bytes INTEGER NOT NULL CHECK(bytes BETWEEN 0 AND 67108864),
 PRIMARY KEY(interaction_id,kind,feedback_ordinal,ordinal),
 FOREIGN KEY(interaction_id,feedback_ordinal) REFERENCES delivery_feedback(interaction_id,ordinal),
 CHECK((kind='feedback' AND feedback_ordinal IS NOT NULL) OR (kind IN('interaction','answer') AND feedback_ordinal IS NULL))
);
CREATE UNIQUE INDEX interaction_attachments_no_feedback ON interaction_attachments(interaction_id,kind,ordinal) WHERE feedback_ordinal IS NULL;
