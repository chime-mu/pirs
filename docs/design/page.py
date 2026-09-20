#!/usr/bin/env python3
"""Build the pirs design page (https://claude.ai/artifact/9gZcPT15MuH5bCqbUSdneU) from
docs/design/*.md. Markdown is embedded verbatim and rendered in the browser with marked;
the repo files stay the source of truth. Run it, then republish the output with the Artifact tool."""
import pathlib, json, datetime

SRC = pathlib.Path("/home/chime/Workspace/pirs/docs/design")
OUT = pathlib.Path("/tmp/claude-1000/-home-chime-Workspace-pirs/64ce14ff-955e-4c70-8bb7-d2e622e6f2a3/scratchpad/pirs-design.html") if pathlib.Path("/tmp/claude-1000").exists() else pathlib.Path("/tmp/pirs-design.html")

LAYERS = [
    ("north-star",    "00-north-star.md",    "North star",    "what pirs is for, in plain words"),
    ("functionality", "10-functionality.md", "Functionality", "what a user can do, as scenarios"),
    ("architecture",  "20-architecture.md",  "Architecture",  "components, crates, edges, phases"),
    ("protocol",      "30-protocol.md",      "Protocol",      "events, slots, requests"),
    ("dsl",           "40-dsl.md",           "DSL",           "policy vocabulary and composition"),
    ("decisions",     "90-decisions.md",     "Decisions",     "numbered, with a status and argument"),
    ("plan",          "PLAN.md",             "Plan",          "phase briefs and who builds what"),
]

def md_block(key, path):
    text = (SRC / path).read_text().replace("</script", "<\\/script")
    return f'<script type="text/markdown" id="md-{key}">\n{text}\n</script>'

nav = "\n".join(
    f'<a class="nav-item" href="#{k}" data-tab="{k}"><span class="nav-name">{name}</span>'
    f'<span class="nav-hint">{hint}</span></a>'
    for k, _, name, hint in LAYERS)
blocks = "\n".join(md_block(k, p) for k, p, _, _ in LAYERS)
tabs = json.dumps([[k, n] for k, _, n, _ in LAYERS])
stamp = datetime.date.today().isoformat()

html = f"""<title>pirs Design Layers</title>
<meta name="description" content="The pirs adaptable-software design, one layer per tab, with its decision log.">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Sans:wght@400;500;600&family=IBM+Plex+Serif:ital,wght@0,400;0,500;1,400&family=IBM+Plex+Mono:wght@400;500&display=swap">
<style>
:root {{
  --bg: #F6F8F6; --panel: #EDF1EE; --ink: #1B2024; --muted: #5B6670; --rule: #D6DDD8;
  --accent: #1F6F78; --accent-ink: #FFFFFF; --code-bg: #E9EEEA; --code-ink: #1B2024;
  --proposed: #8A5A00; --proposed-bg: #FFF1D6; --accepted: #1F6F78; --accepted-bg: #DDEFF0;
  --rejected: #8B2F2F; --rejected-bg: #F7DEDE;
  --sans: "IBM Plex Sans", system-ui, -apple-system, "Segoe UI", sans-serif;
  --serif: "IBM Plex Serif", Georgia, "Times New Roman", serif;
  --mono: "IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, monospace;
}}
@media (prefers-color-scheme: dark) {{
  :root:not([data-theme="light"]) {{
    --bg: #15191B; --panel: #1C2224; --ink: #E3E8E5; --muted: #97A3A6; --rule: #2B3437;
    --accent: #62BCC2; --accent-ink: #0F1A1B; --code-bg: #1E2629; --code-ink: #DDE3E0;
    --proposed: #E0A94A; --proposed-bg: #33270F; --accepted: #62BCC2; --accepted-bg: #10292B;
    --rejected: #E08A8A; --rejected-bg: #3A1B1B;
  }}
}}
:root[data-theme="dark"] {{
  --bg: #15191B; --panel: #1C2224; --ink: #E3E8E5; --muted: #97A3A6; --rule: #2B3437;
  --accent: #62BCC2; --accent-ink: #0F1A1B; --code-bg: #1E2629; --code-ink: #DDE3E0;
  --proposed: #E0A94A; --proposed-bg: #33270F; --accepted: #62BCC2; --accepted-bg: #10292B;
  --rejected: #E08A8A; --rejected-bg: #3A1B1B;
}}
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; }}
body {{ background: var(--bg); color: var(--ink); font-family: var(--serif); font-size: 16px; line-height: 1.55; }}
a {{ color: var(--accent); }}
a:focus-visible, button:focus-visible {{ outline: 2px solid var(--accent); outline-offset: 2px; }}

.shell {{ display: grid; grid-template-columns: 240px minmax(0, 1fr); min-height: 100%; }}
.rail {{ position: sticky; top: env(safe-area-inset-top, 0px); align-self: start; height: 100vh;
  overflow-y: auto; padding: 24px 16px; border-right: 1px solid var(--rule); background: var(--panel);
  font-family: var(--sans); display: flex; flex-direction: column; gap: 4px; }}
.brand {{ font-weight: 600; font-size: 15px; letter-spacing: .02em; margin: 0 8px 4px; }}
.brand small {{ display: block; font-weight: 400; color: var(--muted); font-size: 12px; margin-top: 2px; }}
.rail-rule {{ height: 1px; background: var(--rule); margin: 8px 0 12px; }}
.nav-item {{ display: flex; flex-direction: column; gap: 1px; padding: 8px 10px; border-radius: 6px;
  text-decoration: none; color: var(--ink); border-left: 3px solid transparent; }}
.nav-item:hover {{ background: var(--bg); }}
.nav-item[aria-current="page"] {{ border-left-color: var(--accent); background: var(--bg); }}
.nav-name {{ font-weight: 500; font-size: 14px; }}
.nav-hint {{ font-size: 12px; color: var(--muted); }}
.rail-foot {{ margin-top: auto; padding: 12px 10px 0; font-size: 12px; color: var(--muted); line-height: 1.45; }}
.rail-foot code {{ font-size: 11px; }}

main {{ padding-block: 40px 80px; padding-inline: clamp(16px, 5vw, 64px); }}
.page {{ max-width: 72ch; }}
.page[hidden] {{ display: none; }}
.crumb {{ font-family: var(--sans); font-size: 12px; letter-spacing: .06em; text-transform: uppercase; color: var(--muted); margin-bottom: 8px; }}

.page h1, .page h2, .page h3 {{ font-family: var(--sans); text-wrap: balance; line-height: 1.2; }}
.page h1 {{ font-size: 30px; font-weight: 600; margin: 0 0 18px; }}
.page h2 {{ font-size: 21px; font-weight: 600; margin: 40px 0 12px; padding-top: 16px; border-top: 1px solid var(--rule); }}
.page h3 {{ font-size: 17px; font-weight: 600; margin: 28px 0 8px; }}
.page p {{ margin: 0 0 14px; }}
.page ul, .page ol {{ padding-left: 1.4em; margin: 0 0 14px; }}
.page li {{ margin-bottom: 6px; }}
.page li > p {{ margin-bottom: 6px; }}
.page code {{ font-family: var(--mono); font-size: .88em; background: var(--code-bg); color: var(--code-ink); padding: 1px 5px; border-radius: 4px; }}
.page pre {{ background: var(--code-bg); color: var(--code-ink); border-radius: 6px; padding: 14px 16px; overflow-x: auto; margin: 0 0 18px; font-size: 13px; line-height: 1.45; }}
.page pre code {{ background: none; padding: 0; font-size: inherit; }}
.page strong {{ font-weight: 600; }}
.table-wrap {{ overflow-x: auto; margin: 0 0 18px; }}
.page table {{ border-collapse: collapse; font-family: var(--sans); font-size: 14px; min-width: 100%; }}
.page th, .page td {{ text-align: left; vertical-align: top; padding: 8px 10px; border-bottom: 1px solid var(--rule); }}
.page th {{ font-weight: 600; font-size: 12px; letter-spacing: .05em; text-transform: uppercase; color: var(--muted); border-bottom: 2px solid var(--rule); }}
.page td code {{ white-space: nowrap; }}
.page blockquote {{ margin: 0 0 14px; padding-left: 14px; border-left: 3px solid var(--rule); color: var(--muted); }}

.chip {{ display: inline-block; font-family: var(--sans); font-size: 11px; font-weight: 600; letter-spacing: .06em; text-transform: uppercase;
  padding: 2px 8px; border-radius: 999px; vertical-align: middle; margin-right: 6px; }}
.chip-proposed {{ color: var(--proposed); background: var(--proposed-bg); }}
.chip-accepted {{ color: var(--accepted); background: var(--accepted-bg); }}
.chip-rejected, .chip-superseded {{ color: var(--rejected); background: var(--rejected-bg); }}
.dec {{ padding: 12px 0 4px; border-top: 1px dashed var(--rule); }}
.dec-id {{ font-family: var(--mono); font-size: 13px; color: var(--muted); margin-right: 8px; }}
.tally {{ font-family: var(--sans); font-size: 13px; color: var(--muted); display: flex; gap: 14px; flex-wrap: wrap; margin: -6px 0 22px; }}
.tally b {{ color: var(--ink); font-weight: 600; }}

@media (max-width: 760px) {{
  .shell {{ grid-template-columns: 1fr; }}
  .rail {{ position: sticky; top: env(safe-area-inset-top, 0px); height: auto; border-right: 0; border-bottom: 1px solid var(--rule);
    flex-direction: row; flex-wrap: nowrap; overflow-x: auto; gap: 2px; padding: 10px 12px; z-index: 2; }}
  .brand, .rail-rule, .rail-foot {{ display: none; }}
  .nav-item {{ border-left: 0; border-bottom: 3px solid transparent; border-radius: 6px 6px 0 0; padding: 6px 10px; white-space: nowrap; }}
  .nav-item[aria-current="page"] {{ border-bottom-color: var(--accent); }}
  .nav-hint {{ display: none; }}
  main {{ padding-block: 24px 60px; }}
}}
@media (prefers-reduced-motion: no-preference) {{ .nav-item {{ transition: background .12s; }} }}
</style>

<div class="shell">
  <nav class="rail" aria-label="Design layers">
    <div class="brand">pirs design<small>as of {stamp}</small></div>
    <div class="rail-rule"></div>
    {nav}
    <div class="rail-foot">Source of truth: <code>docs/design/*.md</code> in the repo. This page is a rendering of those files. Leave a comment on any line to send it back.</div>
  </nav>
  <main>
    {"".join(f'<article class="page" id="page-{k}" hidden><div class="crumb">{n} · <span class="crumb-file">{p}</span></div><div class="body"></div></article>' for k, p, n, _ in LAYERS)}
  </main>
</div>

{blocks}

<script src="https://cdnjs.cloudflare.com/ajax/libs/marked/12.0.2/marked.min.js"></script>
<script>
(function () {{
  var TABS = {tabs};
  var keys = TABS.map(function (t) {{ return t[0]; }});
  marked.setOptions({{ gfm: true, breaks: false }});

  function render(key) {{
    var art = document.getElementById('page-' + key);
    var body = art.querySelector('.body');
    if (body.dataset.done) return;
    var src = document.getElementById('md-' + key).textContent;
    body.innerHTML = marked.parse(src);
    body.querySelectorAll('table').forEach(function (t) {{
      var w = document.createElement('div'); w.className = 'table-wrap';
      t.parentNode.insertBefore(w, t); w.appendChild(t);
    }});
    if (key === 'decisions') decorateDecisions(body);
    body.dataset.done = '1';
  }}

  function decorateDecisions(body) {{
    var counts = {{}};
    body.querySelectorAll('p > strong:first-child').forEach(function (s) {{
      var m = /^(D-\\d+)\\s*·\\s*(\\w+)(?:\\s+by\\s+(D-\\d+))?\\s*·\\s*([\\d-]+)\\s*·\\s*(.*)$/.exec(s.textContent);
      if (!m) return;
      var status = m[2].toLowerCase();
      counts[status] = (counts[status] || 0) + 1;
      var p = s.parentNode; p.classList.add('dec'); p.id = m[1].toLowerCase();
      s.innerHTML = '<span class="dec-id">' + m[1] + '</span><span class="chip chip-' + status + '">' + m[2] + (m[3] ? ' by ' + m[3] : '') + '</span>' + m[5] +
        ' <span class="dec-id">' + m[4] + '</span>';
    }});
    var h1 = body.querySelector('h1');
    if (h1) {{
      var t = document.createElement('div'); t.className = 'tally';
      t.innerHTML = Object.keys(counts).sort().map(function (k) {{ return '<span><b>' + counts[k] + '</b> ' + k + '</span>'; }}).join('');
      h1.parentNode.insertBefore(t, h1.nextSibling);
    }}
  }}

  function show(key, push) {{
    if (keys.indexOf(key) < 0) key = keys[0];
    render(key);
    keys.forEach(function (k) {{ document.getElementById('page-' + k).hidden = (k !== key); }});
    document.querySelectorAll('.nav-item').forEach(function (a) {{
      if (a.dataset.tab === key) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
    }});
    try {{ localStorage.setItem('pirs-design-tab', key); }} catch (e) {{}}
    if (push && location.hash !== '#' + key) history.replaceState(null, '', '#' + key);
    window.scrollTo(0, 0);
  }}

  function fromHash() {{
    var h = (location.hash || '').slice(1);
    if (keys.indexOf(h) >= 0) return h;
    if (/^d-\\d+$/.test(h)) {{ show('decisions', false); var el = document.getElementById(h); if (el) el.scrollIntoView(); return null; }}
    var saved = null; try {{ saved = localStorage.getItem('pirs-design-tab'); }} catch (e) {{}}
    return saved && keys.indexOf(saved) >= 0 ? saved : keys[0];
  }}

  window.addEventListener('hashchange', function () {{ var k = fromHash(); if (k) show(k, false); }});
  var first = fromHash(); if (first) show(first, true);
}})();
</script>
"""
OUT.write_text(html)
print(OUT, len(html))
