CREATE TABLE IF NOT EXISTS interaction_attachments(
 interaction_id TEXT NOT NULL REFERENCES interactions(id),kind TEXT NOT NULL CHECK(kind IN('interaction','answer','feedback')),
 feedback_ordinal INTEGER,ordinal INTEGER NOT NULL CHECK(ordinal>=0),id TEXT NOT NULL,name TEXT NOT NULL,content_digest TEXT NOT NULL,content_reference TEXT NOT NULL,bytes INTEGER NOT NULL CHECK(bytes BETWEEN 0 AND 67108864),
 PRIMARY KEY(interaction_id,kind,feedback_ordinal,ordinal),
 FOREIGN KEY(interaction_id,feedback_ordinal) REFERENCES delivery_feedback(interaction_id,ordinal),
 CHECK((kind='feedback' AND feedback_ordinal IS NOT NULL) OR (kind IN('interaction','answer') AND feedback_ordinal IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS interaction_attachments_no_feedback ON interaction_attachments(interaction_id,kind,ordinal) WHERE feedback_ordinal IS NULL;
