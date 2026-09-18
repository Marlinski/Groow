"""Groow: a language model that grows by rewriting its own weights.

Package map (one responsibility per subpackage):

    groow.config    Config dataclass, groow.json on disk
    groow.brain     the neural substrate: base weights + plastic overlay, generation,
                    supervised / policy-gradient steps, consolidation, growth
    groow.memory    episodic memory on disk: episodes, lessons, learning log, drift probes
    groow.learning  the learner: turns experience into gradient steps
                    (passive, feedback, memorize, quiz, play, probe)
    groow.games     rule systems that score actions without a human (self-play rewards)
    groow.harness   the agent loop and tool machinery: registry, built-in tools,
                    self-tools (learning), the conversation loop
    groow.cli       command line: init / chat / memorize / play / ...
"""
__version__ = "0.2.0"
