-- schema --
CREATE TABLE agent_learning_queue (event_id TEXT PRIMARY KEY NOT NULL,agent_id TEXT NOT NULL,sequence INTEGER NOT NULL,payload TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('pending','claimed','delivered')),claimed_by TEXT,claimed_at INTEGER,attempts INTEGER NOT NULL DEFAULT 0,last_error TEXT,created_at INTEGER NOT NULL,delivered_at INTEGER,UNIQUE(agent_id, sequence));
CREATE TABLE agent_learning_state (agent_id TEXT PRIMARY KEY NOT NULL,next_sequence INTEGER NOT NULL DEFAULT 1,enqueue_blocked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE agent_run_messages (session_pk TEXT NOT NULL,message_seq INTEGER NOT NULL,run_id TEXT NOT NULL REFERENCES agent_runs(run_id) ON DELETE CASCADE,PRIMARY KEY(session_pk,message_seq),FOREIGN KEY(session_pk,message_seq) REFERENCES messages(session_pk,seq) ON DELETE CASCADE);
CREATE TABLE agent_runs (run_id TEXT PRIMARY KEY,session_pk TEXT NOT NULL REFERENCES sessions(session_pk) ON DELETE CASCADE,parent_run_id TEXT REFERENCES agent_runs(run_id),retry_of TEXT REFERENCES agent_runs(run_id),primary_agent_id TEXT NOT NULL,executing_agent_id TEXT,executing_agent_name_snapshot TEXT NOT NULL,agent_kind TEXT NOT NULL CHECK(agent_kind IN ('primary','main-delegate','subagent')),task TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('queued','running','completed','failed','cancelled','interrupted')),started_at INTEGER,finished_at INTEGER,tool_count INTEGER NOT NULL DEFAULT 0 CHECK(tool_count >= 0),resolved_model TEXT,resolved_effort TEXT,result TEXT,error TEXT, source_tool_call_id TEXT, dispatch_index INTEGER CHECK(dispatch_index IS NULL OR dispatch_index >= 0), context_active_tokens INTEGER, context_usable_window INTEGER, context_percent_left INTEGER, context_window INTEGER, cache_read_tokens INTEGER, cache_creation_tokens INTEGER, output_tokens INTEGER, cost_models TEXT);
CREATE TABLE artifact_references (id TEXT PRIMARY KEY,artifact_id TEXT NOT NULL,target_session_pk TEXT NOT NULL,shared_from_session_pk TEXT NOT NULL,shared_by TEXT,parent_reference_id TEXT,created_at INTEGER NOT NULL,UNIQUE(artifact_id, target_session_pk));
CREATE TABLE artifact_storage_jobs (id TEXT PRIMARY KEY,status TEXT NOT NULL,source_root TEXT NOT NULL,target_root TEXT NOT NULL,total_count INTEGER NOT NULL DEFAULT 0 CHECK(total_count >= 0),completed_count INTEGER NOT NULL DEFAULT 0 CHECK(completed_count >= 0),current_artifact_id TEXT,error TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE artifacts (id TEXT PRIMARY KEY,source_session_pk TEXT NOT NULL,source_message_seq INTEGER,source_run_id TEXT,creator TEXT NOT NULL CHECK(creator IN ('user','agent')),creator_id TEXT,name TEXT NOT NULL,description TEXT,content_type TEXT,size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),sha256 TEXT NOT NULL,storage_key TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('available','source-archived','deleted')),created_at INTEGER NOT NULL,deleted_at INTEGER);
CREATE TABLE audit (id INTEGER PRIMARY KEY AUTOINCREMENT,gateway TEXT,conversation_id TEXT,actor TEXT,action TEXT,tool TEXT,decision TEXT,at INTEGER, session_pk TEXT, origin TEXT);
CREATE TABLE automation_hook_attempts (run_id TEXT NOT NULL,ordinal INTEGER NOT NULL,started_at INTEGER NOT NULL,finished_at INTEGER,http_status INTEGER,error TEXT,PRIMARY KEY(run_id, ordinal),FOREIGN KEY(run_id) REFERENCES automation_hook_runs(id));
CREATE TABLE automation_hook_runs (id TEXT PRIMARY KEY NOT NULL,hook_id TEXT NOT NULL,status TEXT NOT NULL,envelope_json TEXT NOT NULL,snapshot_json TEXT NOT NULL,session_pk TEXT,error TEXT,attempt_count INTEGER NOT NULL DEFAULT 0,last_http_status INTEGER,queued_at INTEGER NOT NULL,started_at INTEGER,finished_at INTEGER);
CREATE TABLE automation_hooks (id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL COLLATE NOCASE UNIQUE,trigger_kind TEXT NOT NULL,action_kind TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,inbound_path TEXT UNIQUE,config_json TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE background_events (id TEXT PRIMARY KEY NOT NULL,target_session_pk TEXT NOT NULL,kind TEXT NOT NULL,payload TEXT NOT NULL,created_at INTEGER NOT NULL,claimed_by TEXT,delivered_at INTEGER, origin_run_id TEXT);
CREATE TABLE catalog_feed_state (id INTEGER PRIMARY KEY CHECK (id = 1),sequence INTEGER NOT NULL,updated_at INTEGER NOT NULL,outcome TEXT NOT NULL);
CREATE TABLE component_plugin_releases (plugin_id TEXT NOT NULL,version TEXT NOT NULL,source_url TEXT NOT NULL,sha256 TEXT NOT NULL,signing_key_id TEXT NOT NULL,installed_at INTEGER NOT NULL,active INTEGER NOT NULL DEFAULT 0,revoked INTEGER NOT NULL DEFAULT 0,revocation_reason TEXT,PRIMARY KEY (plugin_id, version));
CREATE TABLE component_plugin_storage (plugin_id TEXT NOT NULL,key TEXT NOT NULL,value BLOB NOT NULL,PRIMARY KEY (plugin_id, key));
CREATE TABLE context_checkpoints (id INTEGER PRIMARY KEY AUTOINCREMENT,session_pk TEXT NOT NULL,boundary_seq INTEGER NOT NULL,window_number INTEGER NOT NULL,payload TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE TABLE devices (id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,token_hash TEXT NOT NULL UNIQUE,created_at INTEGER NOT NULL,last_seen INTEGER,revoked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE endpoint_keys (id TEXT PRIMARY KEY,name TEXT NOT NULL DEFAULT '',key TEXT NOT NULL UNIQUE,created_at INTEGER,last_used_at INTEGER);
CREATE TABLE gateway_events (id INTEGER PRIMARY KEY AUTOINCREMENT,gateway_id TEXT NOT NULL,at INTEGER NOT NULL,level TEXT NOT NULL DEFAULT 'info',text TEXT NOT NULL);
CREATE TABLE gateways (id TEXT PRIMARY KEY,name TEXT NOT NULL,kind TEXT NOT NULL,host TEXT,port INTEGER,username TEXT,fs_mode TEXT NOT NULL DEFAULT 'projects',paths TEXT NOT NULL DEFAULT '[]',created_at INTEGER, fingerprint TEXT, device_token TEXT);
CREATE TABLE job_runs (id TEXT PRIMARY KEY,job_id TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'running',started_at INTEGER NOT NULL,finished_at INTEGER,session_pk TEXT,error TEXT,add_lines INTEGER,del_lines INTEGER,note TEXT,log TEXT);
CREATE TABLE jobs (id TEXT PRIMARY KEY,name TEXT NOT NULL,cron TEXT NOT NULL,mode TEXT NOT NULL DEFAULT 'cron',natural_text TEXT NOT NULL DEFAULT '',project_id TEXT NOT NULL,branch TEXT NOT NULL DEFAULT 'main',gateway TEXT NOT NULL DEFAULT 'local',enabled INTEGER NOT NULL DEFAULT 1,prompt TEXT NOT NULL,notify_success INTEGER NOT NULL DEFAULT 0,notify_fail INTEGER NOT NULL DEFAULT 1,created_at INTEGER, pre_check TEXT NOT NULL DEFAULT '', model_override TEXT);
CREATE TABLE mcp_agent_access (server_id TEXT NOT NULL,agent_id TEXT NOT NULL,allowed INTEGER NOT NULL DEFAULT 1,PRIMARY KEY (server_id, agent_id));
CREATE TABLE mcp_servers (id TEXT PRIMARY KEY,name TEXT NOT NULL,kind TEXT NOT NULL DEFAULT 'MCP server',color TEXT NOT NULL DEFAULT '#8B8B8B',description TEXT NOT NULL DEFAULT '',transport TEXT NOT NULL DEFAULT 'stdio',command TEXT,args TEXT NOT NULL DEFAULT '[]',env TEXT NOT NULL DEFAULT '{}',url TEXT,scope TEXT NOT NULL DEFAULT 'global',scope_gateways TEXT NOT NULL DEFAULT '[]',version TEXT,publisher TEXT,status TEXT NOT NULL DEFAULT 'unknown',status_detail TEXT,auth_kind TEXT NOT NULL DEFAULT 'none',auth_detail TEXT,created_at INTEGER);
CREATE TABLE mcp_tools (server_id TEXT NOT NULL,name TEXT NOT NULL,description TEXT NOT NULL DEFAULT '',perm TEXT NOT NULL DEFAULT 'ask',PRIMARY KEY (server_id, name));
CREATE TABLE messages (session_pk TEXT NOT NULL,seq INTEGER NOT NULL,role TEXT NOT NULL,block_type TEXT NOT NULL,payload TEXT NOT NULL,tool_call_id TEXT,status TEXT,tool_kind TEXT,created_at INTEGER NOT NULL, speaker TEXT,PRIMARY KEY (session_pk, seq));
CREATE VIRTUAL TABLE messages_fts USING fts5( text, session_pk UNINDEXED, seq UNINDEXED, tokenize = 'porter unicode61' );
CREATE TABLE 'messages_fts_config'(k PRIMARY KEY, v) WITHOUT ROWID;
CREATE TABLE 'messages_fts_content'(id INTEGER PRIMARY KEY, c0, c1, c2);
CREATE TABLE 'messages_fts_data'(id INTEGER PRIMARY KEY, block BLOB);
CREATE TABLE 'messages_fts_docsize'(id INTEGER PRIMARY KEY, sz BLOB);
CREATE TABLE 'messages_fts_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID;
CREATE TABLE model_effort_preferences (family TEXT NOT NULL,model TEXT NOT NULL,effort TEXT NOT NULL,PRIMARY KEY (family, model));
CREATE TABLE model_status (family TEXT NOT NULL,model TEXT NOT NULL,status TEXT NOT NULL,message TEXT NOT NULL DEFAULT '',tested_at INTEGER NOT NULL,PRIMARY KEY (family, model));
CREATE TABLE native_tool_plans (
                    run_id TEXT PRIMARY KEY NOT NULL REFERENCES agent_runs(run_id) ON DELETE CASCADE,
                    plan_schema_version INTEGER NOT NULL,
                    registry_generation INTEGER NOT NULL,
                    plan_hash TEXT NOT NULL,
                    plan_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                 );
CREATE TABLE native_tool_session_versions (
                    session_pk TEXT PRIMARY KEY NOT NULL REFERENCES sessions(session_pk) ON DELETE CASCADE,
                    version TEXT NOT NULL CHECK(version IN ('v1','v2')),
                    created_at INTEGER NOT NULL
                 );
CREATE TABLE pairing_codes (code_hash TEXT PRIMARY KEY NOT NULL,expires_at INTEGER NOT NULL);
CREATE TABLE plugin_attach_status (plugin_id TEXT PRIMARY KEY NOT NULL,last_attach_at INTEGER NOT NULL,outcome TEXT NOT NULL,reason TEXT);
CREATE TABLE plugin_catalog_cache (id TEXT PRIMARY KEY NOT NULL,manifest_toml TEXT NOT NULL,version TEXT NOT NULL,sequence INTEGER NOT NULL,blocked INTEGER NOT NULL DEFAULT 0,blocked_reason TEXT,fetched_at INTEGER NOT NULL);
CREATE TABLE plugin_installs (plugin_id TEXT PRIMARY KEY NOT NULL,kind TEXT NOT NULL,source_spec TEXT NOT NULL,resolved_commit TEXT,fingerprint TEXT NOT NULL,installed_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,pinned INTEGER NOT NULL DEFAULT 0,pin_reason TEXT,trust_tier TEXT NOT NULL,trust_ack_at INTEGER,trust_ack_summary TEXT);
CREATE TABLE "plugin_oauth_clients" (plugin_id TEXT PRIMARY KEY NOT NULL,authorize_url TEXT,token_url TEXT,client_id TEXT,updated_at INTEGER NOT NULL);
CREATE TABLE plugin_oauth_profile_clients (plugin_id TEXT NOT NULL,profile_id TEXT NOT NULL,authorize_url TEXT,token_url TEXT,client_id TEXT,updated_at INTEGER NOT NULL,PRIMARY KEY (plugin_id, profile_id));
CREATE TABLE plugin_oauth_profile_tokens (plugin_id TEXT NOT NULL,profile_id TEXT NOT NULL,token_json TEXT NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY (plugin_id, profile_id));
CREATE TABLE plugin_oauth_tokens (plugin_id TEXT PRIMARY KEY NOT NULL,token_json TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE project_bindings (gateway TEXT NOT NULL,workspace_id TEXT NOT NULL,project_id TEXT NOT NULL,PRIMARY KEY (gateway, workspace_id));
CREATE TABLE projects (project_id TEXT PRIMARY KEY,name TEXT,workdir TEXT NOT NULL,source TEXT,model TEXT,effort TEXT,perm_mode TEXT NOT NULL DEFAULT 'default',created_at INTEGER);
CREATE TABLE provider_connections (id TEXT PRIMARY KEY,provider TEXT NOT NULL,auth_type TEXT NOT NULL DEFAULT 'api_key',label TEXT NOT NULL DEFAULT '',priority INTEGER NOT NULL DEFAULT 0,enabled INTEGER NOT NULL DEFAULT 1,data TEXT NOT NULL DEFAULT '{}',created_at INTEGER,updated_at INTEGER);
CREATE TABLE provider_turns (session_pk TEXT NOT NULL,seq INTEGER NOT NULL,role TEXT NOT NULL,payload TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY (session_pk, seq));
CREATE TABLE request_log (id TEXT PRIMARY KEY,ts INTEGER NOT NULL,connection_id TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,client_format TEXT NOT NULL,input_tokens INTEGER NOT NULL DEFAULT 0,output_tokens INTEGER NOT NULL DEFAULT 0,status_code INTEGER NOT NULL,duration_ms INTEGER NOT NULL,error TEXT);
CREATE TABLE session_automation_origins (session_pk TEXT PRIMARY KEY NOT NULL,kind TEXT NOT NULL,hook_id TEXT NOT NULL,run_id TEXT NOT NULL,depth INTEGER NOT NULL);
CREATE TABLE session_context (session_pk TEXT PRIMARY KEY NOT NULL,payload TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE session_prompt_queue (id TEXT PRIMARY KEY NOT NULL,session_pk TEXT NOT NULL,position INTEGER NOT NULL,payload TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('pending','claimed')) DEFAULT 'pending',created_at INTEGER NOT NULL,UNIQUE(session_pk, position));
CREATE TABLE session_route_state (session_pk TEXT PRIMARY KEY NOT NULL,requested_model TEXT NOT NULL,resolved_provider TEXT NOT NULL,resolved_family TEXT NOT NULL,resolved_model TEXT NOT NULL,effective_effort TEXT,connection_id TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE session_runtime_settings (session_pk TEXT PRIMARY KEY NOT NULL REFERENCES sessions(session_pk) ON DELETE CASCADE,model TEXT,effort TEXT,updated_at INTEGER NOT NULL);
CREATE TABLE session_surfaces (gateway TEXT NOT NULL,conversation_id TEXT NOT NULL,session_pk TEXT NOT NULL,PRIMARY KEY (gateway, conversation_id));
CREATE TABLE "sessions" (
                session_pk TEXT PRIMARY KEY,
                project_id TEXT,
                agent_session_id TEXT,
                worktree_path TEXT,
                branch TEXT,
                title TEXT,
                status TEXT NOT NULL DEFAULT 'idle',
                created_at INTEGER,
                last_active INTEGER,
                started_by TEXT,
                resume_attempts INTEGER NOT NULL DEFAULT 0,
                branch_owned INTEGER NOT NULL DEFAULT 1,
                perm_mode TEXT NOT NULL DEFAULT 'default',
                kind TEXT NOT NULL DEFAULT 'project',
                speaker TEXT,
                agent TEXT,
                parent_session_pk TEXT
            , primary_agent_id TEXT, primary_agent_snapshot TEXT, archived_at INTEGER);
CREATE TABLE settings (key TEXT PRIMARY KEY,value TEXT);
CREATE TABLE todos (session_pk TEXT NOT NULL,pos INTEGER NOT NULL,content TEXT NOT NULL,status TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY (session_pk, pos));
CREATE TABLE tool_policies (project_id TEXT NOT NULL,tool TEXT NOT NULL,decision TEXT NOT NULL,PRIMARY KEY (project_id, tool));
CREATE TABLE usage_daily (day TEXT NOT NULL,connection_id TEXT NOT NULL,model TEXT NOT NULL,requests INTEGER NOT NULL DEFAULT 0,input_tokens INTEGER NOT NULL DEFAULT 0,output_tokens INTEGER NOT NULL DEFAULT 0,PRIMARY KEY (day, connection_id, model));
CREATE INDEX agent_run_messages_run_idx ON agent_run_messages(run_id,message_seq);
CREATE INDEX agent_runs_dispatch_idx ON agent_runs(session_pk,parent_run_id,source_tool_call_id,dispatch_index);
CREATE INDEX agent_runs_parent_idx ON agent_runs(session_pk,parent_run_id,started_at);
CREATE INDEX agent_runs_status_idx ON agent_runs(session_pk,status);
CREATE INDEX artifact_references_artifact_idx ON artifact_references(artifact_id);
CREATE INDEX artifact_references_target_idx ON artifact_references(target_session_pk, created_at);
CREATE INDEX artifacts_source_session_idx ON artifacts(source_session_pk, created_at);
CREATE INDEX idx_agent_learning_delivery ON agent_learning_queue(agent_id, status, sequence);
CREATE INDEX idx_automation_hook_runs_hook ON automation_hook_runs(hook_id, queued_at DESC);
CREATE INDEX idx_background_events_origin ON background_events(origin_run_id, delivered_at);
CREATE INDEX idx_background_events_target ON background_events(target_session_pk, delivered_at);
CREATE UNIQUE INDEX idx_component_plugin_releases_active ON component_plugin_releases(plugin_id) WHERE active=1;
CREATE INDEX idx_context_checkpoints_session ON context_checkpoints(session_pk, boundary_seq);
CREATE INDEX idx_gateway_events ON gateway_events(gateway_id, at);
CREATE INDEX idx_job_runs_job ON job_runs(job_id, started_at);
CREATE INDEX idx_messages_session ON messages(session_pk, seq);
CREATE INDEX idx_provider_turns_session ON provider_turns(session_pk, seq);
CREATE INDEX idx_request_log_conn ON request_log(connection_id, ts);
CREATE INDEX idx_request_log_ts ON request_log(ts);
CREATE INDEX idx_session_prompt_queue_pending ON session_prompt_queue(session_pk, status, position);
CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages BEGIN DELETE FROM messages_fts WHERE session_pk = old.session_pk AND seq = old.seq; END;
CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages WHEN new.role IN ('user','assistant') AND new.block_type='text' AND json_extract(new.payload,'$.text') IS NOT NULL BEGIN INSERT INTO messages_fts(text, session_pk, seq) VALUES (json_extract(new.payload,'$.text'), new.session_pk, new.seq); END;
-- seed --
INSERT INTO "messages_fts_config"("k", "v") VALUES ('version', 4);
INSERT INTO "messages_fts_data"("id", "block") VALUES (1, X'');
INSERT INTO "messages_fts_data"("id", "block") VALUES (10, X'00000000000000');
