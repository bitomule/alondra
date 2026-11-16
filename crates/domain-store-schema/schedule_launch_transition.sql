CREATE TRIGGER schedule_launch_transition BEFORE UPDATE ON schedule_runs
 WHEN OLD.launch IS NOT NEW.launch AND NOT (
   (OLD.launch IS NULL AND NEW.launch IS NOT NULL AND OLD.state='launching'
     AND NEW.state='sent' AND json_extract(NEW.launch,'$.state')='pending')
   OR (json_extract(OLD.launch,'$.state')='pending'
     AND json_extract(NEW.launch,'$.state') IN ('dispatching','failed'))
   OR (json_extract(OLD.launch,'$.state')='dispatching'
     AND json_extract(NEW.launch,'$.state') IN ('handed','failed','uncertain'))
   OR (json_extract(OLD.launch,'$.state')='uncertain'
     AND json_extract(NEW.launch,'$.state') IN ('handed','failed'))
 )
 BEGIN SELECT RAISE(ABORT,'schedule launch transition is invalid'); END;
