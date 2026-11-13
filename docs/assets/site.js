(function(){
  "use strict";
  var ICON = {"mute":"🔇","vol-down":"🔉","vol-up":"🔊","mic-mute":"🎙️","bright-down":"🔅","bright-up":"🔆","kbd-light":"⌨️","display":"🖥️","touchpad":"🖱️","airplane":"✈️","camera":"📷","lock":"🔒","sleep":"🌙","screenshot":"✂️","calculator":"🧮","settings":"⚙️","search":"🔍","app":"🗔","fan":"🌀","performance":"⚡","refresh-rate":"🔄","fnlock":"🔐","call":"📞","call-end":"📵","favorites":"⭐","emoji":"😊","copilot":"✨","ai":"✨","play-pause":"⏯️","prev":"⏮️","next":"⏭️","stop":"⏹️","home":"⇱","end":"⇲","pgup":"⇞","pgdn":"⇟","insert":"⎀","prtsc":"⎙","scrlk":"⇳","pause":"⏸️","break":"⏸️","sysrq":"⎙","numlock":"🔢","rotate":"🔃","battery":"🔋","wifi":"📶","privacy":"🛡️","power":"⏻","info":"ℹ️","other":"•"};
  var TOP = ["Esc","F1","F2","F3","F4","F5","F6","F7","F8","F9","F10","F11","F12"];
  var root = document.documentElement, profiles = [], vendor = "", dataVersion = "";

  // ── language ──
  function lang(){ return root.getAttribute("data-lang"); }
  function T(en, ko){ return lang() === "ko" ? (ko || en || "") : (en || ko || ""); }
  function setLang(l){
    root.setAttribute("data-lang", l); root.setAttribute("lang", l);
    document.getElementById("langBtn").textContent = l === "ko" ? "English" : "한국어";
    document.getElementById("q").placeholder = T("Model, model code or machine type (e.g. P1503, T14 Gen 4, 21HM)", "모델명·모델 코드·머신 타입 (예: P1503, T14 Gen 4, 21HM)");
    try { localStorage.setItem("lang", l); } catch(e){}
    render(); check();
  }
  document.getElementById("langBtn").addEventListener("click", function(){ setLang(lang() === "ko" ? "en" : "ko"); });

  // ── helpers ──
  function esc(s){ return String(s == null ? "" : s).replace(/[&<>"']/g, function(c){ return {"&":"&amp;","<":"&lt;",">":"&gt;","\"":"&quot;","'":"&#39;"}[c]; }); }
  function safeUrl(u){ return /^https:\/\//i.test(u || "") ? u : "#"; }
  function rx(p){ try { return new RegExp(p, "i"); } catch(e){ return null; } }

  // Same rules as Core/Matcher.cs: machine type 100, model code 90, family/version 80; manufacturer required.
  function score(dev, p){
    var m = p.match || {};
    if (!(m.manufacturer || []).some(function(r){ var x = rx(r); return x && x.test(dev.manufacturer || ""); })) return null;
    var prod = dev.product || "", best = 0, why = "";
    if (prod.length >= 4 && (m.mtm || []).some(function(t){ return t.toUpperCase() === prod.substr(0,4).toUpperCase(); })) { best = 100; why = T("machine type ", "머신 타입 ") + prod.substr(0,4).toUpperCase(); }
    if (best < 90) (m.product || []).some(function(f){
      var x = rx("(?<![A-Za-z0-9])(?:" + f + ")"); if (!x) return false;
      var w = x.test(dev.baseboard || "") ? dev.baseboard : x.test(prod) ? prod : null;
      if (w) { best = 90; why = T("model code in ", "모델 코드 ") + "\"" + w + "\""; return true; } return false;
    });
    if (best < 80) (m.family || []).some(function(f){
      var x = rx(f); if (!x) return false;
      var w = x.test(dev.version || "") ? dev.version : x.test(dev.family || "") ? dev.family : null;
      if (w) { best = 80; why = T("model name ", "모델명 ") + "\"" + w + "\""; return true; } return false;
    });
    return best ? { p: p, score: best, why: why } : null;
  }

  function searchText(p){
    var m = p.match || {};
    return [p.vendor, p.series, p.name, p.years].concat(m.product || [], m.mtm || [], m.family || []).join(" ").toLowerCase();
  }

  function matchesQuery(p, q){
    if (!q) return true;
    var words = q.toLowerCase().split(/\s+/).filter(Boolean), hay = searchText(p);
    if (words.every(function(w){ return hay.indexOf(w) >= 0; })) return true;
    // also try the query as an SMBIOS string ("ThinkPad T14 Gen 4", "21HMCTO1WW", "P1503CVA")
    var dev = { manufacturer: p.vendor === "ASUS" ? "ASUSTeK COMPUTER INC." : "LENOVO", product: q, baseboard: q, version: q, family: q };
    return !!score(dev, p);
  }

  function keyCaps(keys){
    keys = keys.slice().sort(function(a,b){ var i = TOP.indexOf(a.key), j = TOP.indexOf(b.key); return (i<0?99:i) - (j<0?99:j); });
    return '<div class="caps">' + keys.map(function(k){
      var note = T(k.note_en, k.note_ko);
      return '<div class="cap"' + (note ? ' title="' + esc(note) + '"' : '') + '><span class="k">' + esc(k.key) + (note ? " *" : "") + '</span><span class="i">' + (ICON[k.icon] || "") + '</span><span class="t">' + esc(T(k.en, k.ko)) + '</span></div>';
    }).join("") + '</div>';
  }

  function comboList(keys){
    return '<div class="combos">' + keys.map(function(k){
      var note = T(k.note_en, k.note_ko);
      return '<div class="combo"' + (note ? ' title="' + esc(note) + '"' : '') + '><span class="kbd">Fn</span>+<span class="kbd">' + esc(k.key) + '</span><span>' + (ICON[k.icon] || "") + '</span><span class="t">' + esc(T(k.en, k.ko)) + (note ? " *" : "") + '</span></div>';
    }).join("") + '</div>';
  }

  function card(p){
    var keys = p.keys || [], top = keys.filter(function(k){ return k.row === "top"; }), combo = keys.filter(function(k){ return k.row !== "top"; });
    var ok = p.verified !== "partial";
    var notes = keys.filter(function(k){ return T(k.note_en, k.note_ko); }).map(function(k){ return '<li><b>' + esc((k.row === "top" ? "" : "Fn+") + k.key) + '</b> — ' + esc(T(k.note_en, k.note_ko)) + '</li>'; });
    if (T(p.notes_en, p.notes_ko)) notes.push('<li>' + esc(T(p.notes_en, p.notes_ko)) + '</li>');
    var m = p.match || {}, pretty = function(r){ return String(r).replace(/\\b|\^|\$/g, "").replace(/\(\?:/g, "(").replace(/\\s[+*]?/g, " ").replace(/\\/g, ""); }, codes = [].concat((m.product || []).map(pretty), (m.mtm || []).map(function(x){ return "MT " + x; }), (m.family || []).map(pretty));
    var fl = p.fnLock || {};
    return '<details class="model" id="m-' + esc(p.id) + '"><summary>' +
      '<span class="vendor">' + esc(p.vendor) + '</span><span class="mname">' + esc(p.name) + '</span>' +
      (p.years ? '<span class="years">' + esc(p.years) + '</span>' : '') +
      '<span class="badge ' + (ok ? 'ok">' + T("Official manual", "공식 설명서 확인") : 'part">' + T("Partly verified", "일부 확인")) + '</span><span class="chev">▶</span></summary>' +
      '<div class="mbody">' +
      (top.length ? '<div class="label">' + T("Top row", "윗줄 키") + '</div>' + keyCaps(top) : '') +
      (combo.length ? '<div class="label">' + T("Hold Fn and press", "Fn을 누른 채로") + '</div>' + comboList(combo) : '') +
      (fl.combo ? '<div class="label">' + T("Fn Lock", "Fn 잠금") + '</div><div><span class="kbd">' + esc(fl.combo).replace(/\+/g, '</span>+<span class="kbd">') + '</span> &nbsp;' + esc(T(fl.default_en, fl.default_ko)) + '</div>' : '') +
      (notes.length ? '<ul class="notes">' + notes.join("") + '</ul>' : '') +
      '<div class="label">' + T("Detected by", "자동 인식 기준") + '</div><div class="codes">' + esc(codes.join(" · ")) + '</div>' +
      '<div class="label">' + T("Sources", "출처") + '</div><ul class="srcs">' + (p.sources || []).map(function(s){
        return '<li><a href="' + esc(safeUrl(s.url)) + '" target="_blank" rel="noopener noreferrer">' + esc(s.title || s.url) + '</a>' + (s.kind === "community" ? ' <span class="small">(' + T("community", "커뮤니티") + ')</span>' : s.kind === "device-test" ? ' <span class="small">(' + T("tested on a real device", "실기 확인") + ')</span>' : '') + '</li>';
      }).join("") + '</ul></div></details>';
  }

  function render(){
    var list = document.getElementById("list");
    if (!profiles.length) return;
    var q = document.getElementById("q").value.trim();
    var shown = profiles.filter(function(p){ return (!vendor || p.vendor === vendor) && matchesQuery(p, q); });
    list.innerHTML = shown.length ? shown.map(card).join("") : '<div class="empty">' + T("No matching model. Request it on GitHub — see the FAQ below.", "맞는 모델이 없어요. 아래 FAQ를 보고 GitHub로 요청해 주세요.") + '</div>';
    document.getElementById("count").textContent = T(shown.length + " of " + profiles.length, profiles.length + "개 중 " + shown.length + "개");
    var by = function(v){ return profiles.filter(function(p){ return p.vendor === v; }).length; };
    var models = profiles.reduce(function(n, p){ var m = p.match || {}; return n + (m.product || []).length + (m.mtm || []).length; }, 0);
    document.getElementById("stats").innerHTML =
      '<span class="stat"><b>' + profiles.length + '</b>' + T("profiles", "개 프로필") + '</span>' +
      '<span class="stat"><b>' + by("ASUS") + '</b>ASUS</span><span class="stat"><b>' + by("Lenovo") + '</b>Lenovo</span>' +
      '<span class="stat"><b>' + models + '</b>' + T("model codes / machine types", "개 모델 코드·머신 타입") + '</span>' +
      (dataVersion ? '<span class="stat">' + T("data ", "데이터 ") + esc(dataVersion) + '</span>' : '');
  }

  document.getElementById("q").addEventListener("input", render);
  Array.prototype.forEach.call(document.querySelectorAll(".chip"), function(b){
    b.addEventListener("click", function(){
      vendor = b.getAttribute("data-vendor");
      Array.prototype.forEach.call(document.querySelectorAll(".chip"), function(x){ x.setAttribute("aria-pressed", x === b ? "true" : "false"); });
      render();
    });
  });

  // ── checker ──
  function parsePaste(text){
    var d = {};
    text.split(/\r?\n/).forEach(function(line){ var m = line.match(/^\s*([A-Za-z]+)\s*:\s*(.*?)\s*$/); if (m) d[m[1].toLowerCase()] = m[2]; });
    return { manufacturer: d.systemmanufacturer || d.vendor || "", product: d.systemproductname || d.name || "", family: d.systemfamily || "", version: d.systemversion || d.version || "", baseboard: d.baseboardproduct || "" };
  }
  function check(){
    var out = document.getElementById("checkResult"), text = document.getElementById("paste").value;
    if (!text.trim() || !profiles.length){ out.innerHTML = ""; return; }
    var dev = parsePaste(text);
    if (!dev.manufacturer && !dev.product){ out.innerHTML = '<div class="result warn">' + T("Couldn't read the output — paste the whole result of the command above.", "결과를 읽지 못했어요. 위 명령의 결과 전체를 붙여 넣어 주세요.") + '</div>'; return; }
    var c = profiles.map(function(p){ return score(dev, p); }).filter(Boolean).sort(function(a,b){ return b.score - a.score; });
    if (!c.length){ out.innerHTML = '<div class="result warn">' + T("Not in the list yet — the app will ask you to pick a model with the same keyboard. Please request it on GitHub.", "아직 목록에 없어요. 앱이 키보드가 같은 모델을 고르게 안내해요. GitHub로 추가를 요청해 주세요.") + '</div>'; return; }
    if (c.length > 1 && c[1].score === c[0].score){ out.innerHTML = '<div class="result warn">' + T("Several profiles match equally, so the app will ask you to choose: ", "여러 프로필이 똑같이 맞아서 앱이 고르게 물어봐요: ") + c.filter(function(x){ return x.score === c[0].score; }).map(function(x){ return '<a href="#m-' + esc(x.p.id) + '">' + esc(x.p.name) + '</a>'; }).join(", ") + '</div>'; return; }
    out.innerHTML = '<div class="result ok">✅ ' + T("Detected automatically as ", "자동으로 인식돼요: ") + '<a href="#m-' + esc(c[0].p.id) + '" data-open="' + esc(c[0].p.id) + '"><b>' + esc(c[0].p.name) + '</b></a> <span class="small">(' + esc(c[0].why) + ')</span></div>';
  }
  document.getElementById("paste").addEventListener("input", check);
  function openHash(){
    if (location.hash.indexOf("#m-") !== 0) return;
    var el = document.getElementById(location.hash.slice(1));
    if (el){ el.open = true; el.scrollIntoView(); }
  }
  window.addEventListener("hashchange", openHash);
  document.addEventListener("click", function(e){
    var a = e.target.closest && e.target.closest('a[href^="#m-"]');
    if (!a) return;
    var el = document.getElementById(a.getAttribute("href").slice(1));
    if (el) el.open = true;
  });

  // ── load ──
  function load(urls){
    if (!urls.length){ document.getElementById("list").innerHTML = '<div class="empty">' + T("Couldn't load the model list.", "모델 목록을 불러오지 못했어요.") + '</div>'; return; }
    fetch(urls[0], { cache: "no-cache" }).then(function(r){ if (!r.ok) throw 0; return r.json(); }).then(function(d){
      dataVersion = d.dataVersion || "";
      profiles = (d.profiles || []).slice().sort(function(a,b){ return (a.vendor + a.name).localeCompare(b.vendor + b.name); });
      render(); check();
      openHash();
    }).catch(function(){ load(urls.slice(1)); });
  }

  var saved = null; try { saved = localStorage.getItem("lang"); } catch(e){}
  setLang(saved || ((navigator.language || "ko").toLowerCase().indexOf("ko") === 0 ? "ko" : "en"));
  load(["profiles.json", "../data/profiles.json"]);
})();
