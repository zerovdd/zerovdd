// @ts-check
import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import sitemap from '@astrojs/sitemap';
import mermaid from 'astro-mermaid';

const docsSite = process.env.DOCS_SITE || 'https://zerovdd.dev';
const docsBase = process.env.DOCS_BASE?.replace(/\/+$/, '') || '';

function prefixBaseLinks(base) {
  const prefix = base.replace(/\/+$/, '');
  return {
    name: 'prefix-base-links',
    hooks: {
      'astro:build:done': ({ dir }) => {
        if (!prefix) return;
        const root = fileURLToPath(dir);
        const escaped = prefix.slice(1).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        const re = new RegExp(`href="/(?!/|${escaped}(?:/|"|#))`, 'g');
        const visit = (d) => {
          for (const entry of readdirSync(d, { withFileTypes: true })) {
            const full = join(d, entry.name);
            if (entry.isDirectory()) {
              visit(full);
            } else if (entry.name.endsWith('.html')) {
              const html = readFileSync(full, 'utf8');
              const updated = html.replace(re, `href="${prefix}/`);
              if (updated !== html) writeFileSync(full, updated);
            }
          }
        };
        visit(root);
      },
    },
  };
}

export default defineConfig({
  site: docsSite,
  ...(docsBase ? { base: docsBase } : {}),
  integrations: [
    ...(docsBase ? [prefixBaseLinks(docsBase)] : []),
    mermaid({
      mermaidConfig: {
        themeCSS: [
          'text.activeText0, text.activeText1, text.activeText2, text.activeText3 { fill: var(--zv-mermaid-active-text) !important; }',
          'text.doneText0, text.doneText1, text.doneText2, text.doneText3 { fill: var(--zv-mermaid-done-text) !important; }',
          'text.taskTextOutsideLeft, text.taskTextOutsideRight { fill: var(--zv-mermaid-outside-text) !important; }',
        ].join('\n'),
      },
    }),
    sitemap(),
    starlight({
      title: 'ZeroVDD',
      description:
        'Windows 11 UMDF 2 / IddCx 1.10 Virtual Display Driver, Session-0 service, CLI, and native system tray companion engineered in Rust.',
      favicon: '/favicon.svg',
      logo: {
        src: './src/assets/logo.svg',
      },
      head: [
        {
          tag: 'link',
          attrs: {
            rel: 'apple-touch-icon',
            href: '/apple-touch-icon.png',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image',
            content: `${docsSite.replace(/\/+$/, '')}${docsBase}/og-card.png`,
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:width',
            content: '1200',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:height',
            content: '630',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:alt',
            content: 'ZeroVDD - Windows 11 Virtual Display Driver',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:card',
            content: 'summary_large_image',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:image',
            content: `${docsSite.replace(/\/+$/, '')}${docsBase}/og-card.png`,
          },
        },
      ],
      customCss: [
        '@fontsource-variable/inter',
        '@fontsource-variable/jetbrains-mono',
        './src/styles/zerovdd-theme.css',
      ],
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/zerovdd/zerovdd',
        },
      ],
      editLink: {
        baseUrl: 'https://github.com/zerovdd/zerovdd/edit/main/docs/',
      },
      lastUpdated: true,
      sidebar: [
        {
          label: 'Getting Started',
          items: [
            { slug: 'getting-started/installation' },
            { slug: 'getting-started/quickstart' },
            { slug: 'getting-started/sunshine' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { slug: 'guides/displays-and-layouts' },
            { slug: 'guides/edid-hdr-and-color' },
            { slug: 'guides/gpu-selection' },
            { slug: 'guides/headless-hosts' },
            { slug: 'guides/system-tray' },
            { slug: 'guides/troubleshooting' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { slug: 'reference/cli' },
            { slug: 'reference/configuration' },
            { slug: 'reference/doctor-and-etw' },
            { slug: 'reference/ipc-and-security' },
          ],
        },
        {
          label: 'Architecture',
          items: [
            { slug: 'architecture/overview' },
            { slug: 'architecture/driver-internals' },
            { slug: 'architecture/session-zero-trampoline' },
            { slug: 'architecture/testing-policy' },
          ],
        },
        {
          label: 'Release & Governance',
          collapsed: true,
          items: [
            { slug: 'release/signing-and-release' },
            { slug: 'release/signpath-onboarding' },
            { slug: 'release/code-signing-policy' },
          ],
        },
      ],
    }),
  ],
});
