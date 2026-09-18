---
name: tictactoe
description: Plays games of noughts and crosses against itself and records every move with whether it led to a win, a draw or a loss. Use when you want practice rather than conversation, or when you have been asked to work on your own play.
metadata:
  origin: shipped-with-groow
  kind: practice
---

# Practising

```
tictactoe play --rounds 8    play, and log each decision with its reward
tictactoe --help             the rest
```

Every move becomes a decision in your activity log with a reward attached: winning is worth
one, a draw a fifth of that, losing or playing an illegal square costs one. The learning passes
turn those into policy samples later. You are not scoring yourself here; the game's rules are.

## Why this is not a conversation

The moves are generated at low priority, behind anything a person is waiting for, and they go
straight to the brain rather than through a turn. A game must never come before someone
talking to you.

## What to expect

Early rounds are mostly illegal moves and losses. That is the point: the samples with negative
rewards are what teach the difference. Look at the score across rounds rather than at any one
game.
