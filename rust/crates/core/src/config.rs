//! `groow.json`.
//!
//! Every field has a default, unknown fields are ignored, and a malformed file is an error
//! rather than a silent fall back to defaults: starting with the wrong settings because a
//! comma was missing is worse than refusing to start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

fn d_model() -> String { "Qwen/Qwen3-4B".into() }
fn d_state() -> String { "state".into() }
fn d_max_seq() -> usize { 32768 }
fn d_train_max() -> usize { 4096 }
fn d_attn() -> String { "sdpa".into() }
fn d_rank() -> usize { 32 }
fn d_alpha() -> usize { 64 }
fn d_targets() -> Vec<String> {
    ["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"]
        .iter().map(|s| s.to_string()).collect()
}
fn d_lr() -> f64 { 5e-5 }
fn d_grad_clip() -> f64 { 1.0 }
fn d_true() -> bool { true }
fn d_role_weights() -> BTreeMap<String, f64> {
    [("user", 0.5), ("assistant", 1.0), ("tool", 0.0), ("system", 0.0), ("tool_call_only", 0.2)]
        .iter().map(|(k, v)| (k.to_string(), *v)).collect()
}
fn d_rehearsal() -> usize { 2 }
fn d_ctx_kept() -> usize { 8 }
fn d_probe_every() -> usize { 25 }
fn d_nap_max() -> usize { 8 }
fn d_trace() -> bool { true }
fn d_trace_keep() -> usize { 200 }
fn d_idle_nap_max() -> usize { 32 }
fn d_sleep_steps() -> usize { 300 }
fn d_sleep_hours() -> f64 { 12.0 }
fn d_sleep_replay() -> usize { 30 }
fn d_sleep_drift() -> f64 { 0.5 }
fn d_internalize() -> usize { 20 }
fn d_curiosity_mode() -> String { "agentic".into() }
fn d_sense_idle() -> f64 { 10.0 }
fn d_sense_idle_max() -> f64 { 90.0 }
fn d_sense_items() -> usize { 6 }
fn d_sense_passes() -> usize { 3 }
fn d_max_thoughts() -> usize { 4 }
fn d_thought_reminder() -> usize { 3 }
fn d_gen_batch() -> usize { 8 }
fn d_tool_timeout() -> u64 { 120 }
fn d_skill_check_timeout() -> u64 { 60 }
fn d_turn_timeout() -> f64 { 900.0 }
fn d_thought_timeout() -> f64 { 3600.0 }
fn d_hold() -> f64 { 300.0 }
fn d_judge() -> String { "laya".into() }
fn d_halflife() -> f64 { 1800.0 }
fn d_host() -> String { "127.0.0.1".into() }
fn d_port() -> u16 { 7373 }
fn d_python_timeout() -> u64 { 30 }
fn d_temperature() -> f64 { 0.7 }
fn d_top_p() -> f64 { 0.8 }
fn d_top_k() -> usize { 20 }
fn d_max_new() -> usize { 768 }
fn d_max_rounds() -> u32 { 10 }
fn d_brain_port() -> u16 { 7374 }
fn d_agent_user() -> String { "groow".into() }

/// Everything settable, in the order it appears in the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // model
    pub model_id: String,
    pub state_dir: String,
    pub max_seq_len: usize,
    pub train_max_len: usize,
    pub attn_implementation: String,

    // plasticity
    pub lora_rank: usize,
    pub lora_alpha: usize,
    pub lora_targets: Vec<String>,
    pub lr: f64,
    pub weight_decay: f64,
    pub grad_clip: f64,

    // passive learning
    pub passive_learning: bool,
    pub role_weights: BTreeMap<String, f64>,
    pub learn_from_bad_turns: bool,
    pub rehearsal_k: usize,
    pub context_messages_kept: usize,

    // active learning
    pub probe_every: usize,
    /// Whether to write down exactly what was sent to the brain, for every request.
    ///
    /// On, because the cost is disk and the alternative is guessing about why it said
    /// something. `trace_keep` is how many runs are worth keeping.
    #[serde(default = "d_trace")]
    pub trace: bool,
    #[serde(default = "d_trace_keep")]
    pub trace_keep: usize,
    pub nap_max_samples: usize,
    pub idle_nap_max_samples: usize,

    // sleep
    pub sleep_every_steps: usize,
    pub sleep_every_hours: f64,
    pub sleep_replay_steps: usize,
    pub sleep_max_drift: f64,
    pub keep_previous_base: bool,

    // identity
    pub identity_in_prompt: bool,
    pub sleep_internalize_steps: usize,

    // curiosity
    pub curiosity: bool,
    pub curiosity_mode: String,
    pub sense_idle_minutes: f64,
    pub sense_idle_max_minutes: f64,
    pub sense_items: usize,
    pub sense_passes: usize,
    pub feeds: Vec<String>,

    // mind
    pub max_thoughts: usize,
    pub thought_reminder_every: usize,
    pub learn_from_thoughts: bool,
    pub gen_max_batch: usize,

    // self-extension
    pub tool_timeout: u64,
    pub skill_check_timeout: u64,

    // processes
    pub turn_timeout: f64,
    pub thought_timeout: f64,

    // mentor attention
    pub hold_seconds: f64,

    // limbic
    pub judge: String,
    pub mood_halflife_s: f64,

    // gateway
    pub api_host: String,
    pub api_port: u16,

    // harness
    pub allow_python: bool,
    pub allow_shell: bool,
    pub home_dir: String,
    pub python_timeout: u64,

    // generation
    pub enable_thinking: bool,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: usize,
    pub max_new_tokens: usize,
    pub max_tool_rounds: u32,

    // the Rust split: where the Python side listens, and who the mind runs as
    pub brain_port: u16,
    pub agent_user: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model_id: d_model(), state_dir: d_state(), max_seq_len: d_max_seq(),
            train_max_len: d_train_max(), attn_implementation: d_attn(),
            lora_rank: d_rank(), lora_alpha: d_alpha(), lora_targets: d_targets(),
            lr: d_lr(), weight_decay: 0.0, grad_clip: d_grad_clip(),
            passive_learning: d_true(), role_weights: d_role_weights(),
            learn_from_bad_turns: false, rehearsal_k: d_rehearsal(),
            context_messages_kept: d_ctx_kept(), probe_every: d_probe_every(),
            trace: d_trace(), trace_keep: d_trace_keep(),
            nap_max_samples: d_nap_max(), idle_nap_max_samples: d_idle_nap_max(),
            sleep_every_steps: d_sleep_steps(), sleep_every_hours: d_sleep_hours(),
            sleep_replay_steps: d_sleep_replay(), sleep_max_drift: d_sleep_drift(),
            keep_previous_base: d_true(), identity_in_prompt: d_true(),
            sleep_internalize_steps: d_internalize(), curiosity: d_true(),
            curiosity_mode: d_curiosity_mode(), sense_idle_minutes: d_sense_idle(),
            sense_idle_max_minutes: d_sense_idle_max(), sense_items: d_sense_items(),
            sense_passes: d_sense_passes(), feeds: Vec::new(),
            max_thoughts: d_max_thoughts(), thought_reminder_every: d_thought_reminder(),
            learn_from_thoughts: false, gen_max_batch: d_gen_batch(),
            tool_timeout: d_tool_timeout(),
            skill_check_timeout: d_skill_check_timeout(), turn_timeout: d_turn_timeout(),
            thought_timeout: d_thought_timeout(), hold_seconds: d_hold(),
            judge: d_judge(), mood_halflife_s: d_halflife(),
            api_host: d_host(), api_port: d_port(), allow_python: d_true(), allow_shell: d_true(), home_dir: String::new(),
            python_timeout: d_python_timeout(), enable_thinking: false,
            temperature: d_temperature(), top_p: d_top_p(), top_k: d_top_k(),
            max_new_tokens: d_max_new(), max_tool_rounds: d_max_rounds(),
            brain_port: d_brain_port(), agent_user: d_agent_user(),
        }
    }
}

impl Config {
    /// Load, or take the defaults when the file is absent. A file that exists but does not
    /// parse is an error: silently running on defaults would be worse.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        match crate::paths::read_opt(path)? {
            None => Ok(Config::default()),
            Some(s) => serde_json::from_str(&s)
                .map_err(|e| anyhow::anyhow!("{} is not valid config: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut s = serde_json::to_string_pretty(self)?;
        s.push('\n');
        crate::paths::atomic_write(path, s.as_bytes())?;
        Ok(())
    }

    pub fn state(&self) -> PathBuf {
        PathBuf::from(&self.state_dir)
    }

    /// The loopback address the gateway listens on. `0.0.0.0` is rewritten for display so a
    /// pasted url actually works.
    pub fn gateway_url(&self) -> String {
        let host = if self.api_host == "0.0.0.0" { "127.0.0.1" } else { &self.api_host };
        format!("http://{host}:{}", self.api_port)
    }

    pub fn brain_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.brain_port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let d = tempfile::tempdir().unwrap();
        let c = Config::load(&d.path().join("groow.json")).unwrap();
        assert_eq!(c.model_id, "Qwen/Qwen3-4B");
        assert_eq!(c.api_port, 7373);
        assert_eq!(c.max_tool_rounds, 10);
    }

    #[test]
    fn unknown_keys_are_ignored_and_missing_keys_keep_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("groow.json");
        std::fs::write(&p, r#"{"api_port": 9999, "from_the_future": 1}"#).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.api_port, 9999);
        assert_eq!(c.model_id, "Qwen/Qwen3-4B", "untouched keys keep their default");
    }

    #[test]
    fn a_broken_file_refuses_to_load() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("groow.json");
        std::fs::write(&p, "{\"api_port\": 9999,,}").unwrap();
        let e = Config::load(&p).unwrap_err().to_string();
        assert!(e.contains("not valid config"), "unhelpful error: {e}");
    }

    #[test]
    fn it_round_trips_through_a_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("groow.json");
        let c = Config {
            model_id: "Qwen/Qwen3-1.7B".into(),
            feeds: vec!["https://example.org/rss".into()],
            ..Default::default()
        };
        c.save(&p).unwrap();
        let back = Config::load(&p).unwrap();
        assert_eq!(back.model_id, c.model_id);
        assert_eq!(back.feeds, c.feeds);
        assert_eq!(back.role_weights["assistant"], 1.0);
    }

    #[test]
    fn the_wildcard_host_is_rewritten_for_display() {
        let c = Config { api_host: "0.0.0.0".into(), ..Default::default() };
        assert_eq!(c.gateway_url(), "http://127.0.0.1:7373");
    }
}
