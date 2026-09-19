"""Everything that needs the GPU, and everything that changes what is on it.

The core, the agent loop and the window are in `rust/`. This package is the other side: the
brain that generates, and the two systems that decide what it becomes.

    serve        the brain as a service: generation, training and consolidation, one queue,
                 one GPU, so a training step is simply another kind of work in the line
    learn        the conductor the core invokes between turns and at night
    limbic       how a turn felt: free sensors from what happened, and a frozen judge for how
                 a person reacted. Nothing here rates itself.
    hippocampus  what was felt becomes something to practise
    brain        the weights: the frozen base, the overlay that moves, and the steps that move it
    learning     the trainer, which is the one place gradients come from
    memory       reading the conversation back: the journal, and a day as whole turns
    stats        the core's measurements, readable only from the root side
    proto        generated from nervous_system/proto; not written by hand
"""
__version__ = "0.3.0"
