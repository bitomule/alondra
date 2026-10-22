CREATE TRIGGER IF NOT EXISTS lifecycle_input_immutable BEFORE UPDATE ON lifecycle_inputs
 WHEN OLD.body IS NOT NULL OR NEW.body IS NULL OR NEW.task_id<>OLD.task_id OR NEW.input_id<>OLD.input_id
 BEGIN SELECT RAISE(ABORT,'lifecycle input is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_command_retained BEFORE DELETE ON provider_commands
 BEGIN SELECT RAISE(ABORT,'provider command evidence is retained'); END;
