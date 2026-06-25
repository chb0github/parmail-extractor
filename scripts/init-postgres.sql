-- Parmail Extractor PostgreSQL schema

-- Jobs table
CREATE TABLE IF NOT EXISTS jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id VARCHAR(255) UNIQUE NOT NULL,
    status VARCHAR(50) NOT NULL DEFAULT 'queued',
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    manifest JSONB,
    error TEXT,
    INDEX (job_id),
    INDEX (status),
    INDEX (created_at)
);

-- Manifest metadata table
CREATE TABLE IF NOT EXISTS manifests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id VARCHAR(255) NOT NULL UNIQUE,
    email_id VARCHAR(255) NOT NULL,
    model_id VARCHAR(255) NOT NULL,
    processed_at TIMESTAMPTZ NOT NULL,
    mail_pieces_count INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (job_id) REFERENCES jobs(job_id) ON DELETE CASCADE,
    INDEX (email_id),
    INDEX (model_id)
);

-- Images storage table
CREATE TABLE IF NOT EXISTS images (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id VARCHAR(255) NOT NULL,
    piece_id VARCHAR(255) NOT NULL,
    image_type VARCHAR(50) NOT NULL,
    data BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (job_id, piece_id, image_type),
    FOREIGN KEY (job_id) REFERENCES jobs(job_id) ON DELETE CASCADE,
    INDEX (job_id),
    INDEX (piece_id)
);

-- Mail pieces index for faster queries
CREATE TABLE IF NOT EXISTS mail_pieces (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id VARCHAR(255) NOT NULL,
    piece_id VARCHAR(255) NOT NULL,
    from_address JSONB,
    to_address JSONB,
    mail_type VARCHAR(100) NOT NULL,
    confidence NUMERIC(3,2),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (job_id) REFERENCES jobs(job_id) ON DELETE CASCADE,
    INDEX (job_id),
    INDEX (mail_type),
    INDEX (piece_id)
);

-- Job queue for async processing
CREATE TABLE IF NOT EXISTS job_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id VARCHAR(255) NOT NULL UNIQUE,
    priority INT NOT NULL DEFAULT 0,
    email_data TEXT NOT NULL,
    model_id VARCHAR(255) NOT NULL,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    attempts INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL DEFAULT 3,
    error_msg TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (job_id) REFERENCES jobs(job_id) ON DELETE CASCADE,
    INDEX (status),
    INDEX (priority),
    INDEX (created_at)
);

-- Create indexes for performance
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
CREATE INDEX IF NOT EXISTS idx_jobs_created_at ON jobs(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_manifests_job_id ON manifests(job_id);
CREATE INDEX IF NOT EXISTS idx_mail_pieces_job_id ON mail_pieces(job_id);
CREATE INDEX IF NOT EXISTS idx_job_queue_status ON job_queue(status);
