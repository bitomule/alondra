CREATE TRIGGER IF NOT EXISTS provider_observation_immutable BEFORE UPDATE ON provider_observations
 BEGIN SELECT RAISE(ABORT,'provider observation evidence is immutable'); END;
CREATE TRIGGER IF NOT EXISTS provider_observation_retained BEFORE DELETE ON provider_observations
 BEGIN SELECT RAISE(ABORT,'provider observation evidence is retained'); END;
