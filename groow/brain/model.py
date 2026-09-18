"""The Brain: a base transformer plus a plastic low-rank overlay that is trained
continuously, and periodically *consolidated* (merged) into the base weights.

Why this split?  On a 32 GB V100 (fp16 only, no bf16) full fine-tuning of a
1.7B model with Adam does not fit and is numerically fragile.  A LoRA overlay is
tiny, fast and stable, yet still a real change of the function computed by the
network.  Merging it into the base weights and re-initialising a fresh overlay
gives us short-term plasticity (the overlay) and long-term memory (the base),
which is roughly how we describe biological consolidation during sleep.

Everything the brain learns is persisted under `state/`:
  state/base/      the consolidated model (HF format, fp16 safetensors)
  state/plastic/   the current overlay + optimizer state
  state/brain.json bookkeeping (steps, consolidations, rank)
"""
from __future__ import annotations

import json
import shutil
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable

import torch
import torch.nn.functional as F
from transformers import AutoModelForCausalLM, AutoTokenizer, TextStreamer, StoppingCriteria, StoppingCriteriaList
from peft import LoraConfig, PeftModel, get_peft_model

from ..config import Config
from .chatfmt import Sample, render

Messages = list[dict]


@dataclass
class Decision:
    """One decision made by the policy during a game: a prompt, what it said, and
    what the rules thought of it."""
    prompt_ids: list[int]
    completion_ids: list[int]
    reward: float
    advantage: float = 0.0
    meta: dict | None = None


class Brain:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
        self.dtype = torch.float16 if self.device.type == "cuda" else torch.float32
        self.tok = None
        self.model = None          # PeftModel
        self.opt = None
        self.scaler = None
        self.meta = {"steps": 0, "consolidations": 0, "rank": cfg.lora_rank,
                     "tokens_seen": 0, "born": time.time(), "growth": []}
        self._lock = threading.Lock()

    # ------------------------------------------------------------------ paths
    @property
    def base_dir(self) -> Path: return self.cfg.state / "base"
    @property
    def plastic_dir(self) -> Path: return self.cfg.state / "plastic"
    @property
    def meta_path(self) -> Path: return self.cfg.state / "brain.json"

    # ------------------------------------------------------------------ setup
    def initialize(self, force: bool = False) -> None:
        """First birth: copy the pretrained model into state/base."""
        if self.base_dir.exists() and not force:
            return
        tok = AutoTokenizer.from_pretrained(self.cfg.model_id)
        model = AutoModelForCausalLM.from_pretrained(self.cfg.model_id, dtype=self.dtype)
        tmp = self.base_dir.with_suffix(".tmp")
        if tmp.exists():
            shutil.rmtree(tmp)
        model.save_pretrained(tmp, safe_serialization=True)
        tok.save_pretrained(tmp)
        if self.base_dir.exists():
            shutil.rmtree(self.base_dir)
        tmp.rename(self.base_dir)
        if self.plastic_dir.exists():
            shutil.rmtree(self.plastic_dir)
        self.meta_path.write_text(json.dumps(self.meta, indent=2))

    def load(self) -> "Brain":
        if not self.base_dir.exists():
            self.initialize()
        if self.meta_path.exists():
            self.meta.update(json.loads(self.meta_path.read_text()))
        self.tok = AutoTokenizer.from_pretrained(self.base_dir)
        self.tok.padding_side = "left"
        base = AutoModelForCausalLM.from_pretrained(
            self.base_dir, dtype=self.dtype, device_map=self.device.type,
            attn_implementation=self.cfg.attn_implementation)
        base.config.use_cache = True
        adapter = self.plastic_dir / "adapter"
        if adapter.exists():
            self.model = PeftModel.from_pretrained(base, adapter, is_trainable=True)
        else:
            self.model = get_peft_model(base, self._lora_config(self.meta.get("rank", self.cfg.lora_rank)))
        self._prepare_trainable()
        self._make_optimizer()
        opt_state = self.plastic_dir / "optimizer.pt"
        if opt_state.exists():
            try:
                st = torch.load(opt_state, map_location=self.device, weights_only=False)
                self.opt.load_state_dict(st["opt"])
                self.scaler.load_state_dict(st["scaler"])
            except Exception as e:  # rank changed etc.
                print(f"[brain] optimizer state not restored: {e}")
        return self

    def _lora_config(self, rank: int) -> LoraConfig:
        return LoraConfig(r=rank, lora_alpha=int(self.cfg.lora_alpha * rank / self.cfg.lora_rank),
                          lora_dropout=0.0, target_modules=self.cfg.lora_targets, task_type="CAUSAL_LM")

    def _prepare_trainable(self) -> None:
        # master weights of the plastic part in fp32; base stays fp16 and frozen
        for _, p in self.model.named_parameters():
            if p.requires_grad and p.dtype != torch.float32:
                p.data = p.data.float()
        self.model.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
        self.model.enable_input_require_grads()
        self.model.eval()

    def _make_optimizer(self) -> None:
        params = [p for p in self.model.parameters() if p.requires_grad]
        self.opt = torch.optim.AdamW(params, lr=self.cfg.lr, weight_decay=self.cfg.weight_decay, betas=(0.9, 0.99))
        self.scaler = torch.amp.GradScaler("cuda", enabled=self.device.type == "cuda")

    def trainable_parameters(self) -> int:
        return sum(p.numel() for p in self.model.parameters() if p.requires_grad)

    def total_parameters(self) -> int:
        return sum(p.numel() for p in self.model.parameters())

    # ------------------------------------------------------------------ persistence
    def save(self) -> None:
        with self._lock:
            self.plastic_dir.mkdir(parents=True, exist_ok=True)
            self.model.save_pretrained(self.plastic_dir / "adapter")
            torch.save({"opt": self.opt.state_dict(), "scaler": self.scaler.state_dict()},
                       self.plastic_dir / "optimizer.pt")
            self.meta_path.write_text(json.dumps(self.meta, indent=2))

    # ------------------------------------------------------------------ generation
    def prompt_text(self, messages: Messages, tools=None, enable_thinking: bool | None = None) -> str:
        if enable_thinking is None:
            enable_thinking = self.cfg.enable_thinking
        return render(self.tok, messages, tools=tools, add_generation_prompt=True, enable_thinking=enable_thinking)

    @torch.no_grad()
    def generate(self, messages: Messages, tools=None, on_text: Callable[[str], None] | None = None,
                 max_new_tokens: int | None = None, enable_thinking: bool | None = None,
                 temperature: float | None = None, should_stop: Callable[[], bool] | None = None) -> str:
        """Generate one assistant turn directly (single-threaded use). Multi-threaded
        callers go through brain.server.ServedBrain instead."""
        text = self.prompt_text(messages, tools, enable_thinking)
        temp = self.cfg.temperature if temperature is None else temperature
        with self._lock:
            return self._generate_text(text, max_new_tokens or self.cfg.max_new_tokens, temp, on_text, should_stop)

    def _generate_text(self, prompt: str, max_new_tokens: int, temperature: float,
                       on_text: Callable[[str], None] | None, should_stop: Callable[[], bool] | None) -> str:
        """Caller holds self._lock. Raises Interrupted if should_stop fires."""
        self.model.eval()
        enc = self.tok(prompt, return_tensors="pt", add_special_tokens=False).to(self.device)
        streamer = _CallbackStreamer(self.tok, on_text) if on_text else None
        flag = _InterruptCriteria(should_stop) if should_stop else None
        out = self.model.generate(
            **enc, max_new_tokens=max_new_tokens, **_sampling(temperature, self.cfg.top_p, self.cfg.top_k),
            streamer=streamer, pad_token_id=self.tok.pad_token_id, use_cache=True,
            stopping_criteria=StoppingCriteriaList([flag]) if flag else None)
        if flag is not None and flag.fired:
            raise Interrupted()
        gen = out[0, enc["input_ids"].shape[1]:]
        return self.tok.decode(gen, skip_special_tokens=True)

    def _generate_texts(self, prompts: list[str], max_new_tokens: int, temperature: float,
                        should_stop: Callable[[], bool] | None) -> list[str]:
        """Several prompts as one left-padded batch (caller holds self._lock)."""
        self.model.eval()
        enc = self.tok(prompts, return_tensors="pt", padding=True, add_special_tokens=False).to(self.device)
        flag = _InterruptCriteria(should_stop) if should_stop else None
        out = self.model.generate(
            **enc, max_new_tokens=max_new_tokens, **_sampling(temperature, self.cfg.top_p, self.cfg.top_k),
            pad_token_id=self.tok.pad_token_id, use_cache=True,
            stopping_criteria=StoppingCriteriaList([flag]) if flag else None)
        if flag is not None and flag.fired:
            raise Interrupted()
        plen = enc["input_ids"].shape[1]
        return [self.tok.decode(out[i, plen:], skip_special_tokens=True) for i in range(len(prompts))]

    @torch.no_grad()
    def generate_batch(self, prompts: list[str], max_new_tokens: int, temperature: float = 1.0
                       ) -> list[tuple[list[int], list[int], str]]:
        """Sample completions for many prompts at once (left padded).
        Returns (prompt_ids, completion_ids, completion_text) per prompt."""
        self.model.eval()
        enc = self.tok(prompts, return_tensors="pt", padding=True, add_special_tokens=False).to(self.device)
        out = self.model.generate(
            **enc, max_new_tokens=max_new_tokens, **_sampling(temperature, 1.0, 0),
            pad_token_id=self.tok.pad_token_id, use_cache=True)
        plen = enc["input_ids"].shape[1]
        results = []
        eos = set(self._eos_ids())
        for i, p in enumerate(prompts):
            pid = [t for t, m in zip(enc["input_ids"][i].tolist(), enc["attention_mask"][i].tolist()) if m]
            comp = out[i, plen:].tolist()
            cut = []
            for t in comp:
                cut.append(t)           # keep the eos so the policy learns to stop
                if t in eos:
                    break
            results.append((pid, cut, self.tok.decode(cut, skip_special_tokens=True)))
        return results

    def _eos_ids(self) -> list[int]:
        e = self.model.generation_config.eos_token_id
        if e is None:
            e = self.tok.eos_token_id
        return list(e) if isinstance(e, (list, tuple)) else [e]

    # ------------------------------------------------------------------ measuring
    @torch.no_grad()
    def sample_loss(self, sample: Sample) -> float:
        """Mean per-token loss over the weighted tokens of a sample (no learning)."""
        self.model.eval()
        ids = torch.tensor([sample.input_ids], device=self.device)
        w = torch.tensor([sample.weights], device=self.device)
        with torch.autocast(self.device.type, dtype=self.dtype, enabled=self.device.type == "cuda"):
            logits = self.model(input_ids=ids, use_cache=False).logits
        return _weighted_ce(logits, ids, w).item()

    # ------------------------------------------------------------------ learning
    def sft_step(self, samples: Iterable[Sample]) -> float:
        """One optimizer step of supervised learning over `samples` (gradient
        accumulation, one sample at a time to keep memory flat). Tokens with a
        negative weight receive an *unlikelihood* loss instead (push away)."""
        samples = [s for s in samples if s.learnable_tokens > 0]
        if not samples:
            return float("nan")
        with self._lock:
            self.model.train()
            total = 0.0
            for s in samples:
                ids = torch.tensor([s.input_ids], device=self.device)
                w = torch.tensor([s.weights], device=self.device)
                with torch.autocast(self.device.type, dtype=self.dtype, enabled=self.device.type == "cuda"):
                    logits = self.model(input_ids=ids, use_cache=False).logits
                loss = _weighted_ce(logits, ids, w) / len(samples)
                self.scaler.scale(loss).backward()
                total += loss.item()
                self.meta["tokens_seen"] += s.learnable_tokens
            self._optimizer_step()
            self.model.eval()
        return total

    def pg_step(self, decisions: list[Decision], micro_batch: int = 4) -> float:
        """REINFORCE / GRPO-style policy gradient: raise the log-probability of
        completions with positive advantage, lower it for negative ones."""
        decisions = [d for d in decisions if d.completion_ids and d.advantage != 0.0]
        if not decisions:
            return 0.0   # every reward equal: nothing to learn from this round
        with self._lock:
            self.model.train()
            total = 0.0
            n = len(decisions)
            for i in range(0, n, micro_batch):
                chunk = decisions[i:i + micro_batch]
                seqs = [d.prompt_ids + d.completion_ids for d in chunk]
                L = max(len(s) for s in seqs)
                pad = self.tok.pad_token_id
                ids = torch.full((len(chunk), L), pad, device=self.device)
                mask = torch.zeros((len(chunk), L), device=self.device)
                adv = torch.tensor([d.advantage for d in chunk], device=self.device)
                for j, (d, s) in enumerate(zip(chunk, seqs)):
                    ids[j, :len(s)] = torch.tensor(s, device=self.device)
                    mask[j, len(d.prompt_ids):len(s)] = 1.0
                with torch.autocast(self.device.type, dtype=self.dtype, enabled=self.device.type == "cuda"):
                    logits = self.model(input_ids=ids, attention_mask=(ids != pad).long() | (mask > 0).long(),
                                        use_cache=False).logits
                logp = F.log_softmax(logits[:, :-1].float(), dim=-1)
                tgt = ids[:, 1:]
                tok_logp = logp.gather(-1, tgt.unsqueeze(-1)).squeeze(-1)
                m = mask[:, 1:]
                seq_logp = (tok_logp * m).sum(-1) / m.sum(-1).clamp(min=1)
                loss = -(adv * seq_logp).sum() / n
                self.scaler.scale(loss).backward()
                total += loss.item()
            self._optimizer_step()
            self.model.eval()
        return total

    def _optimizer_step(self) -> None:
        self.scaler.unscale_(self.opt)
        torch.nn.utils.clip_grad_norm_([p for p in self.model.parameters() if p.requires_grad], self.cfg.grad_clip)
        self.scaler.step(self.opt)
        self.scaler.update()
        self.opt.zero_grad(set_to_none=True)
        self.meta["steps"] += 1

    # ------------------------------------------------------------------ consolidation & growth
    def consolidate(self, new_rank: int | None = None, keep_previous: bool = False) -> dict:
        """Merge the plastic overlay into the base weights (long-term memory),
        write the new base to disk, and start a fresh overlay. With
        `keep_previous` the outgoing base is kept as state/base.prev (rollback)."""
        with self._lock:
            rank = new_rank or self.meta.get("rank", self.cfg.lora_rank)
            t = time.time()
            base = self.model.merge_and_unload()
            base.config.use_cache = True
            tmp = self.base_dir.with_suffix(".tmp")
            if tmp.exists():
                shutil.rmtree(tmp)
            base.save_pretrained(tmp, safe_serialization=True)
            self.tok.save_pretrained(tmp)
            prev = self.base_dir.with_name("base.prev")
            if keep_previous:
                if prev.exists():
                    shutil.rmtree(prev)
                self.base_dir.rename(prev)
            else:
                shutil.rmtree(self.base_dir)
            tmp.rename(self.base_dir)
            if self.plastic_dir.exists():
                shutil.rmtree(self.plastic_dir)
            self.model = get_peft_model(base, self._lora_config(rank))
            self._prepare_trainable()
            self._make_optimizer()
            self.meta["consolidations"] += 1
            self.meta["rank"] = rank
            self.meta_path.write_text(json.dumps(self.meta, indent=2))
            torch.cuda.empty_cache()
            return {"consolidations": self.meta["consolidations"], "rank": rank, "seconds": round(time.time() - t, 1)}

    def grow_rank(self, new_rank: int) -> dict:
        """Function-preserving capacity growth: consolidate, then attach a wider overlay.
        The network computes exactly the same function right after growing; it just
        has more directions in which it can now change."""
        if new_rank <= self.meta.get("rank", self.cfg.lora_rank):
            return {"error": f"new rank must exceed current rank {self.meta.get('rank')}"}
        info = self.consolidate(new_rank=new_rank)
        self.meta["growth"].append({"kind": "rank", "to": new_rank, "at_step": self.meta["steps"], "ts": time.time()})
        self.meta_path.write_text(json.dumps(self.meta, indent=2))
        info["trainable_parameters"] = self.trainable_parameters()
        return info

    def status(self) -> dict:
        mem = torch.cuda.memory_allocated() / 1e9 if self.device.type == "cuda" else 0.0
        return {**self.meta, "trainable_parameters": self.trainable_parameters(),
                "total_parameters": self.total_parameters(), "gpu_memory_gb": round(mem, 2),
                "model_id": self.cfg.model_id}


def _sampling(temperature: float, top_p: float, top_k: int) -> dict:
    if temperature <= 0:
        return {"do_sample": False, "temperature": None, "top_p": None, "top_k": None}
    return {"do_sample": True, "temperature": temperature, "top_p": top_p, "top_k": top_k}


def _weighted_ce(logits: torch.Tensor, ids: torch.Tensor, weights: torch.Tensor) -> torch.Tensor:
    """Weighted next-token loss. Positive weights: cross-entropy. Negative weights:
    unlikelihood -log(1 - p) scaled by |w|. Normalised by total |weight|."""
    logits = logits[:, :-1].float()
    tgt = ids[:, 1:]
    w = weights[:, 1:]
    logp = F.log_softmax(logits, dim=-1).gather(-1, tgt.unsqueeze(-1)).squeeze(-1)
    pos = w.clamp(min=0)
    neg = (-w).clamp(min=0)
    ce = -logp
    ul = -torch.log1p(-logp.exp().clamp(max=1 - 1e-4))
    denom = (pos + neg).sum().clamp(min=1e-6)
    return ((pos * ce) + (neg * ul)).sum() / denom


class Interrupted(Exception):
    """Generation was preempted by something more urgent."""


class _InterruptCriteria(StoppingCriteria):
    def __init__(self, should_stop):
        self.should_stop, self.fired = should_stop, False

    def __call__(self, input_ids, scores, **kwargs):
        if self.should_stop():
            self.fired = True
        return torch.full((input_ids.shape[0],), self.fired, dtype=torch.bool, device=input_ids.device)


class _CallbackStreamer(TextStreamer):
    def __init__(self, tok, cb):
        super().__init__(tok, skip_prompt=True, skip_special_tokens=True)
        self.cb = cb

    def on_finalized_text(self, text: str, stream_end: bool = False):
        if text:
            self.cb(text)
