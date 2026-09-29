//! The page around the report's sections: escaping and number formats,
//! the color tokens (light and dark, both selected, the change ramp
//! validated as ordered steps against each surface), the styles and the
//! small script for tooltips and sortable, filterable tables. Nothing is
//! loaded from outside the file.

/// HTML-escape text for element content and attribute values.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A whole number with thousands separators: `31,904`.
pub fn fmt_int(v: f64) -> String {
    let n = v.round() as i64;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// A headline number, compacted: `948`, `31.9K`, `1.2M`.
pub fn compact(v: f64) -> String {
    match v.abs() {
        a if a >= 1e6 => format!("{:.1}M", v / 1e6),
        a if a >= 1e4 => format!("{:.1}K", v / 1e3),
        _ => fmt_int(v),
    }
}

/// A number for reading: whole with separators from 100 or when it is
/// whole (a decimal on `1,034.2` is noise), else one decimal.
pub fn num(v: f64) -> String {
    if v.abs() >= 100.0 || (v - v.round()).abs() < 0.05 {
        fmt_int(v)
    } else {
        format!("{v:.1}")
    }
}

/// A count and its noun: `1 file`, `2 files`, `1,034 lines`.
pub fn count(n: f64, one: &str, many: &str) -> String {
    let noun = if (n - 1.0).abs() < 1e-9 { one } else { many };
    format!("{} {noun}", fmt_int(n))
}

/// A measured value and the range missing evidence allows: `12.3`,
/// `1,034` or `12.3–15.0`; one number when both ends read the same.
pub fn interval(b: [f64; 2]) -> String {
    let (lo, hi) = (num(b[0]), num(b[1]));
    if lo == hi { lo } else { format!("{lo}–{hi}") }
}

/// A stat tile: label, value, and a line of context.
pub fn tile(label: &str, value: &str, meta: &str) -> String {
    tile_with(label, value, meta, "")
}

/// A stat tile with more markup below its context line (a meter).
pub fn tile_with(label: &str, value: &str, meta: &str, extra: &str) -> String {
    format!(
        r#"<div class="tile"><div class="t-label">{}</div><div class="t-value">{}</div><div class="t-meta">{}</div>{extra}</div>"#,
        esc(label),
        esc(value),
        esc(meta)
    )
}

/// A section: heading, one line saying what it shows, and its body.
pub fn section(id: &str, title: &str, lede: &str, body: &str) -> String {
    format!(
        r#"<section id="{id}"><h2>{}</h2><p class="lede">{}</p>{body}</section>"#,
        esc(title),
        esc(lede)
    )
}

/// A sortable, filterable table inside a disclosure: the table view of a
/// chart. `rows` hold `(sort value, text)` cells.
pub fn table_view(id: &str, summary: &str, head: &[&str], rows: &[Vec<(f64, String)>]) -> String {
    let mut t = format!(
        r#"<details class="tv"><summary>{}</summary><input class="filter" data-filter="{id}" placeholder="Filter" aria-label="Filter {}"><div class="scroll"><table id="{id}"><thead><tr>"#,
        esc(summary),
        esc(summary)
    );
    for h in head {
        t.push_str(&format!(r#"<th tabindex="0">{}</th>"#, esc(h)));
    }
    t.push_str("</tr></thead><tbody>");
    for r in rows {
        t.push_str("<tr>");
        for (v, text) in r {
            t.push_str(&format!(r#"<td data-v="{v}">{}</td>"#, esc(text)));
        }
        t.push_str("</tr>");
    }
    t + "</tbody></table></div></details>"
}

pub const STYLE: &str = r#"
.viz-root{color-scheme:light;--page:#f9f9f7;--surface:#fcfcfb;--ink:#0b0b0b;--ink2:#52514e;--muted:#898781;
--grid:#e1e0d9;--base:#c3c2b7;--ring:rgba(11,11,11,.10);--accent:#2a78d6;--possible:#86b6ef;--track:#cde2fb;
--b0:#e1e0d9;--b1:#86b6ef;--b2:#5598e7;--b3:#2a78d6;--b4:#1c5cab;--b5:#104281;
--on0:#0b0b0b;--on1:#0b0b0b;--on2:#0b0b0b;--on3:#ffffff;--on4:#ffffff;--on5:#ffffff;
--good:#0ca30c;--warning:#fab219;--serious:#ec835a;--critical:#d03b3b;--n1:#898781;--n2:#c3c2b7}
@media (prefers-color-scheme:dark){:root:where(:not([data-theme="light"])) .viz-root{color-scheme:dark;--page:#0d0d0d;--surface:#1a1a19;
--ink:#ffffff;--ink2:#c3c2b7;--grid:#2c2c2a;--base:#383835;--ring:rgba(255,255,255,.10);--accent:#3987e5;--possible:#184f95;--track:#0d366b;
--b0:#383835;--b1:#184f95;--b2:#256abf;--b3:#3987e5;--b4:#6da7ec;--b5:#b7d3f6;
--on0:#ffffff;--on1:#ffffff;--on2:#ffffff;--on3:#0b0b0b;--on4:#0b0b0b;--on5:#0b0b0b;--n2:#383835}}
:root[data-theme="dark"] .viz-root{color-scheme:dark;--page:#0d0d0d;--surface:#1a1a19;
--ink:#ffffff;--ink2:#c3c2b7;--grid:#2c2c2a;--base:#383835;--ring:rgba(255,255,255,.10);--accent:#3987e5;--possible:#184f95;--track:#0d366b;
--b0:#383835;--b1:#184f95;--b2:#256abf;--b3:#3987e5;--b4:#6da7ec;--b5:#b7d3f6;
--on0:#ffffff;--on1:#ffffff;--on2:#ffffff;--on3:#0b0b0b;--on4:#0b0b0b;--on5:#0b0b0b;--n2:#383835}
*{box-sizing:border-box}
body{margin:0;background:var(--page);color:var(--ink);font:14px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:1120px;margin:0 auto;padding:28px 16px 48px}
header h1{font-size:22px;font-weight:600;margin:0}
.scope{color:var(--ink2);margin:4px 0 20px;overflow-wrap:anywhere}
code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.92em}
h2{font-size:16px;font-weight:600;margin:0}
section{background:var(--surface);border:1px solid var(--ring);border-radius:12px;padding:20px;margin:16px 0}
.lede{color:var(--ink2);margin:2px 0 16px}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px}.tiles.dims{grid-template-columns:repeat(auto-fit,minmax(200px,1fr))}
.tile{background:var(--surface);border:1px solid var(--ring);border-radius:12px;padding:14px 16px}
.t-label{color:var(--ink2);font-size:13px}.t-value{font-size:26px;font-weight:600;margin:2px 0}.t-meta{color:var(--ink2);font-size:12px}
.map{width:100%;height:auto;display:block}.map.narrow{display:none}
.cell rect{stroke:none}.cell:hover rect,.cell:focus rect{filter:brightness(1.08)}.cell:focus{outline:none}.cell:focus rect{stroke:var(--ink);stroke-width:2}
.b0 rect{fill:var(--b0)}.b1 rect{fill:var(--b1)}.b2 rect{fill:var(--b2)}.b3 rect{fill:var(--b3)}.b4 rect{fill:var(--b4)}.b5 rect{fill:var(--b5)}
.b0 .on{fill:var(--on0)}.b1 .on{fill:var(--on1)}.b2 .on{fill:var(--on2)}.b3 .on{fill:var(--on3)}.b4 .on{fill:var(--on4)}.b5 .on{fill:var(--on5)}
.on{font-size:11px;pointer-events:none}.on.sub{opacity:.8}
.group{fill:var(--ink2);font-size:12px;font-weight:600}
.badge{fill:var(--ink);stroke:var(--surface);stroke-width:2}.badge-n{fill:var(--surface);font-size:10px;font-weight:700;text-anchor:middle;pointer-events:none}
.legend{display:flex;flex-wrap:wrap;gap:10px 20px;align-items:center;color:var(--ink2);font-size:12px;margin-top:12px}
.key{display:inline-flex;flex-wrap:wrap;align-items:center;gap:6px 8px}.nw{white-space:nowrap}
.ramp{display:inline-flex;gap:2px}.ramp span{display:flex;flex-direction:column;align-items:center;gap:3px;min-width:38px;font-variant-numeric:tabular-nums}
.ramp i{display:block;align-self:stretch;height:10px}.ramp span:first-child i{border-radius:3px 0 0 3px}.ramp span:last-child i{border-radius:0 3px 3px 0}
.num-badge{display:inline-grid;place-items:center;width:16px;height:16px;border-radius:50%;background:var(--ink);color:var(--surface);font-size:10px;font-weight:700}
.rows{list-style:none;margin:0;padding:0}
.row{display:grid;gap:12px;align-items:center;padding:7px 6px;border-bottom:1px solid var(--grid);border-radius:4px}
.row.head{color:var(--muted);font-size:12px;border-bottom-color:var(--base);border-radius:0}
li.row:hover,li.row:focus{background:var(--page);outline:none}li.row:focus-visible{box-shadow:inset 0 0 0 2px var(--accent)}
.hot .row{grid-template-columns:28px minmax(0,1fr) 72px 96px minmax(120px,30%)}
.pairs .row{grid-template-columns:minmax(0,1fr) 60px 60px minmax(100px,26%)}
.rules .row{grid-template-columns:minmax(0,1fr) 72px minmax(120px,30%)}
.fn .row{grid-template-columns:220px minmax(0,1fr) 72px 88px 96px}
.path{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.dir{color:var(--muted)}
.n{color:var(--ink2);font-variant-numeric:tabular-nums}.r{text-align:right;font-variant-numeric:tabular-nums}
.bar{display:flex;gap:2px;height:10px}.bar i{display:block;height:10px}.bar .k{background:var(--accent)}.bar .p{background:var(--possible)}
.bar i:last-child{border-radius:0 4px 4px 0}
.meter{display:flex;gap:2px;height:8px;background:var(--track);border-radius:0 4px 4px 0;margin-top:10px}
.meter i{display:block;height:8px;background:var(--accent);border-radius:0 4px 4px 0}
.tag{font-size:11px;color:var(--ink2);border:1px solid var(--base);border-radius:999px;padding:0 7px;margin-left:6px;white-space:nowrap}
.status{display:inline-flex;align-items:center;gap:6px;white-space:nowrap}
.dot{width:10px;height:10px;border-radius:50%;display:inline-block;flex:none}
.s-strong{background:var(--good)}.s-weak{background:var(--warning)}.s-unexecuted{background:var(--critical)}.s-executed{background:var(--n1)}.s-unknown{background:var(--n2)}
.stack{display:flex;gap:2px;height:14px;margin:4px 0 10px}.stack i{display:block;height:14px}.stack i:first-child{border-radius:4px 0 0 4px}.stack i:last-child{border-radius:0 4px 4px 0}
.empty{color:var(--ink2);padding:8px 0}
.tv{margin-top:14px}.tv summary{cursor:pointer;color:var(--ink2);font-size:13px}
.filter{width:100%;max-width:320px;padding:6px 10px;margin:10px 0;border:1px solid var(--base);border-radius:8px;background:var(--page);color:var(--ink);font:inherit}
.scroll{max-height:420px;overflow:auto;border:1px solid var(--grid);border-radius:8px}
table{width:100%;border-collapse:collapse;font-size:13px}th,td{text-align:left;padding:6px 10px;border-bottom:1px solid var(--grid);white-space:nowrap}
th{position:sticky;top:0;background:var(--surface);color:var(--ink2);font-weight:600;cursor:pointer;user-select:none}
td[data-v]{font-variant-numeric:tabular-nums}
footer{color:var(--muted);font-size:12px;margin-top:24px}footer p{margin:4px 0}
#tip{position:fixed;pointer-events:none;z-index:10;max-width:340px;background:var(--surface);color:var(--ink2);border:1px solid var(--ring);border-radius:8px;
padding:8px 10px;font-size:12px;box-shadow:0 4px 16px rgba(0,0,0,.14);display:none;white-space:pre-line}
#tip b{color:var(--ink);font-weight:600;display:block;overflow-wrap:anywhere}
@media (max-width:640px){main{padding:20px 16px 40px}section{padding:16px}.hide-s{display:none}
.tile{padding:12px 14px}.t-value{font-size:20px}.map.wide{display:none}.map.narrow{display:block}
.hot .row{grid-template-columns:24px minmax(0,1fr) 56px;row-gap:4px}.hot .row .bar{grid-column:2/-1}
.pairs .row{grid-template-columns:minmax(0,1fr) 48px 52px}.pairs .row .bar{display:none}.path{white-space:normal;overflow-wrap:anywhere}
.rules .row{grid-template-columns:minmax(0,1fr) 56px}.rules .row .bar{grid-column:1/-1}
.fn .row{grid-template-columns:minmax(0,1fr) 64px}.fn .row .path{grid-column:1/-1}.fn .row .r:nth-of-type(n+2){display:none}}
"#;

pub const SCRIPT: &str = r#"
(()=>{const tip=document.getElementById('tip');
function show(el,x,y){const lines=(el.dataset.tip||'').split('\n');tip.textContent='';const b=document.createElement('b');b.textContent=lines[0];tip.appendChild(b);
 tip.appendChild(document.createTextNode(lines.slice(1).join('\n')));tip.style.display='block';
 const w=tip.offsetWidth,h=tip.offsetHeight;tip.style.left=Math.max(8,Math.min(x+14,innerWidth-w-8))+'px';tip.style.top=Math.max(8,Math.min(y+14,innerHeight-h-8))+'px'}
document.querySelectorAll('[data-tip]').forEach(el=>{
 el.addEventListener('pointermove',e=>show(el,e.clientX,e.clientY));el.addEventListener('pointerleave',()=>tip.style.display='none');
 el.addEventListener('focus',()=>{const r=el.getBoundingClientRect();show(el,r.left,r.bottom)});el.addEventListener('blur',()=>tip.style.display='none')});
document.querySelectorAll('th').forEach((th,i)=>{const sort=()=>{const tb=th.closest('table').tBodies[0];const rows=[...tb.rows];
 const asc=th.dataset.asc!=='1';th.dataset.asc=asc?'1':'0';const idx=[...th.parentNode.children].indexOf(th);
 rows.sort((a,b)=>{const x=a.cells[idx],y=b.cells[idx];const nx=parseFloat(x.dataset.v),ny=parseFloat(y.dataset.v);
  const c=(!isNaN(nx)&&!isNaN(ny))?nx-ny:x.textContent.localeCompare(y.textContent);return asc?c:-c});
 rows.forEach(r=>tb.appendChild(r))};th.addEventListener('click',sort);th.addEventListener('keydown',e=>{if(e.key==='Enter')sort()})});
document.querySelectorAll('input[data-filter]').forEach(inp=>inp.addEventListener('input',()=>{const q=inp.value.toLowerCase();
 document.querySelectorAll('#'+inp.dataset.filter+' tbody tr').forEach(r=>{r.style.display=r.textContent.toLowerCase().includes(q)?'':'none'})}));
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_as_people_write_them() {
        assert_eq!(fmt_int(31_904.4), "31,904");
        assert_eq!(compact(26_612.0), "26.6K");
        assert_eq!(
            interval([1_034.2, 1_034.4]),
            "1,034",
            "both ends read the same"
        );
        assert_eq!(interval([12.34, 15.0]), "12.3–15");
        assert_eq!(interval([0.0, 0.0]), "0");
        assert_eq!(count(1.0, "file", "files"), "1 file");
        assert_eq!(count(1_034.0, "line", "lines"), "1,034 lines");
    }
}
