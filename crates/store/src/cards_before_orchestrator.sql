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
