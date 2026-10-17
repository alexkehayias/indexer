const plugin = require('tailwindcss/plugin');

/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ['./src/**/*.{html,js}'],
  darkMode: 'class',
  theme: {
    extend: {
      // Colors resolve to CSS variables declared in input.css so a future dark
      // theme can override them under `.dark` without touching markup.
      colors: {
        teal: {
          DEFAULT: 'var(--color-teal)',
          strong: 'var(--color-teal-strong)',
          bright: 'var(--color-teal-bright)',
          tint: 'var(--color-teal-tint)',
        },
        amber: {
          DEFAULT: 'var(--color-amber)',
          ink: 'var(--color-amber-ink)',
          tint: 'var(--color-amber-tint)',
        },
        rust: {
          DEFAULT: 'var(--color-rust)',
          tint: 'var(--color-rust-tint)',
        },
        moss: {
          DEFAULT: 'var(--color-moss)',
          tint: 'var(--color-moss-tint)',
        },
        mauve: {
          DEFAULT: 'var(--color-mauve)',
          tint: 'var(--color-mauve-tint)',
        },
        cobalt: {
          DEFAULT: 'var(--color-cobalt)',
          tint: 'var(--color-cobalt-tint)',
        },
        card: 'var(--color-card)',
        canvas: 'var(--color-canvas)',
        mist: 'var(--color-mist)',
        line: 'var(--color-line)',
        muted: 'var(--color-muted)',
        ink: 'var(--color-ink)',
        body: 'var(--color-body)',
        code: 'var(--color-code)',
      },
      borderRadius: {
        tile: '20px',
        card: '22px',
        bubble: '18px',
        icon: '13px',
      },
    },
  },
  plugins: [
    plugin(({ addUtilities }) => {
      addUtilities({
        '.scrollbar-none': {
          'scrollbar-color': 'gray transparent',
        },
      });
    }),
  ],
};
