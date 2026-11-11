<script lang="ts">
  // The brand mark: Jodd's name in Thai, the word it comes from ("to jot").
  //
  // The one exception to "UI copy is English only" (DESIGN.md, The Brand Mark
  // Rule): it is a mark, not copy. It appears only on the sign-in screen and
  // in About, it is hidden from screen readers because the English name sits
  // beside it, and this file is the only one the Thai guard lets it live in
  // (designGuards.test.ts, brandMark.test.ts).
  //
  // The glow behind it is the lamp of "The Lamplit Desk" coming on: the one
  // authored motion in the app. The mark itself never moves or fades, so a
  // skipped animation still shows the whole mark.
</script>

<span class="brand-mark" aria-hidden="true"><span class="glyphs">จด</span></span>

<style>
  .brand-mark {
    position: relative;
    display: inline-block;
    /* The glow paints behind the glyphs without escaping into the card. */
    isolation: isolate;
  }

  .brand-mark::before {
    content: '';
    position: absolute;
    inset: calc(var(--size-mark) * -0.5) calc(var(--size-mark) * -0.75);
    z-index: -1;
    background: radial-gradient(closest-side, var(--lamp-glow), transparent);
    pointer-events: none;
    animation: lamp-on var(--dur-focal) var(--ease-out) both;
  }

  .glyphs {
    display: block;
    font-size: var(--size-mark);
    font-weight: var(--weight-bold);
    line-height: var(--leading);
    /* Amber Ink: text is never Lamp Amber (The Two Ambers Rule). */
    color: var(--accent-action);
  }

  /* The lamp warms up: its light grows out from the mark as it brightens. */
  @keyframes lamp-on {
    from {
      opacity: 0;
      transform: scale(0.6);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }

  /* Reduced motion: the light still comes on, but nothing grows. */
  @keyframes lamp-on-still {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .brand-mark::before {
      animation-name: lamp-on-still;
    }
  }
</style>
