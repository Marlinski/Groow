from __future__ import annotations

import json
from dataclasses import dataclass, field, asdict
from pathlib import Path


@dataclass
class Config:
    # --- model -------------------------------------------------------------
    model_id: str = "Qwen/Qwen3-4B"       # 8 GB fp16; Qwen/Qwen3-1.7B is the small/fast alternative
    state_dir: str = "state"
    max_seq_len: int = 32768         # prompt budget for chat (model supports 262k; V100 has no FlashAttention)
    train_max_len: int = 4096        # longest sample a learning step backpropagates through (tail of the conversation)
    attn_implementation: str = "sdpa"

    # --- plasticity (the trainable part) ----------------------------------
    lora_rank: int = 32
    lora_alpha: int = 64
    lora_targets: list[str] = field(default_factory=lambda: [
        "q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"])
    lr: float = 5e-5
    weight_decay: float = 0.0
    grad_clip: float = 1.0

    # --- passive learning (every conversation turn) -----------------------
    passive_learning: bool = True
    role_weights: dict[str, float] = field(default_factory=lambda: {
        "user": 0.5,        # absorb what people tell it
        "assistant": 1.0,   # reinforce what it said (its habits)
        "tool": 0.0,        # tool outputs are transient, do not memorise them
        "system": 0.0,
    })
    rehearsal_k: int = 2          # old episodes replayed alongside each new step (anti-forgetting)
    context_messages_kept: int = 8  # how much conversation context is stored with each episode

    # --- active learning --------------------------------------------------
    memorize_target_loss: float = 0.15
    memorize_max_steps: int = 60
    play_batch: int = 16          # parallel episodes per policy-gradient step
    play_temperature: float = 1.0
    play_max_new_tokens: int = 12
    play_explore: float = 0.25    # fraction of decisions replaced by a random legal action (exploration)
    probe_every: int = 25         # auto-measure drift on the fixed probes every N learning steps
    allow_invented_games: bool = False   # lets the model exec() its own game code. Sandbox first.

    # --- sleep (consolidation policy) --------------------------------------
    sleep_every_steps: int = 300      # a night is due after this many learning steps (0 = manual only)
    sleep_replay_steps: int = 30      # replay steps over the day's episodes and lessons before merging
    sleep_max_drift: float = 0.5      # abort the night (discard overlay) if probe loss rose more than this
    keep_previous_base: bool = True   # keep state/base.prev for `groow rollback` (costs one extra base on disk)

    # --- identity -------------------------------------------------------------
    identity_in_prompt: bool = True   # False once the identity has been internalised enough to drop the text
    sleep_internalize_steps: int = 20 # context-distillation steps per night (0 = off)

    # --- curiosity (idle behaviour) -----------------------------------------
    curiosity: bool = True
    curiosity_mode: str = "agentic"   # "agentic": Groow reads the news through its tools; "pipeline": fixed drill
    sense_idle_minutes: float = 10.0  # idle time in chat before a curiosity pass
    sense_items: int = 6              # news items per pass
    sense_passes: int = 3             # gradient steps per learned fact
    feeds: list[str] = field(default_factory=list)   # empty = groow.senses.news.DEFAULT_FEEDS

    # --- mind (inner thoughts) --------------------------------------------
    max_thoughts: int = 4             # concurrently running inner thoughts
    thought_reminder_every: int = 3   # steps between "a thought is running" reminders to the main thought
    learn_from_thoughts: bool = False # passive-learn on inner monologue too (self-distillation risk)
    gen_max_batch: int = 8            # generation server: max concurrent sequences per forward pass

    # --- self-extension (skills) ---------------------------------------------
    skills_enabled: bool = True        # load state/skills/*.py and expose draft/install tools
    tool_timeout: int = 120            # seconds an I/O tool may run before the loop gives up on it
    skill_check_timeout: int = 60      # sandbox check budget for a draft

    # --- gateway (daemon API) ---------------------------------------------------
    api_host: str = "127.0.0.1"       # 0.0.0.0 inside Docker
    api_port: int = 7373

    # --- harness ----------------------------------------------------------
    workspace_dir: str = "state/workspace"   # the only place file tools may read/write
    allow_python: bool = True                # run_python tool (subprocess with timeout)
    python_timeout: int = 30

    # --- generation -------------------------------------------------------
    enable_thinking: bool = False
    temperature: float = 0.7
    top_p: float = 0.8
    top_k: int = 20
    max_new_tokens: int = 768
    max_tool_rounds: int = 6

    @property
    def state(self) -> Path:
        return Path(self.state_dir)

    @classmethod
    def load(cls, path: str | Path | None = None) -> "Config":
        cfg = cls()
        p = Path(path) if path else Path("groow.json")
        if p.exists():
            data = json.loads(p.read_text())
            for k, v in data.items():
                if hasattr(cfg, k):
                    setattr(cfg, k, v)
        return cfg

    def save(self, path: str | Path = "groow.json") -> None:
        Path(path).write_text(json.dumps(asdict(self), indent=2))
