-- Whole staged jobs reserve their complete root allowance before any model pass.
-- Existing single-pass attempts retain their original conservative envelope.
ALTER TABLE job_attempts ADD COLUMN passes INTEGER NOT NULL DEFAULT 1 CHECK(passes BETWEEN 1 AND 4);
ALTER TABLE job_attempts ADD COLUMN reserved_passes INTEGER NOT NULL DEFAULT 1 CHECK(reserved_passes BETWEEN 1 AND 4);
ALTER TABLE job_attempts ADD COLUMN reserved_input INTEGER NOT NULL DEFAULT 1024 CHECK(reserved_input BETWEEN 1 AND 4096);
ALTER TABLE job_attempts ADD COLUMN reserved_output INTEGER NOT NULL DEFAULT 512 CHECK(reserved_output BETWEEN 1 AND 1024);
ALTER TABLE job_attempts ADD COLUMN reserved_millis INTEGER NOT NULL DEFAULT 120000 CHECK(reserved_millis BETWEEN 1 AND 120000);
ALTER TABLE job_attempts ADD COLUMN hard_until INTEGER NOT NULL DEFAULT 0;
UPDATE job_attempts SET hard_until=charged_at+120000;
