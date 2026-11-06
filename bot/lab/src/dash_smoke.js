// Render smoke test for dash.html (run by the dash tests when node is installed): a minimal DOM, the page script, one
// snapshot; prints the section titles in page order ("[fold]" = a folded group) and fails on any script error.
const fs=require("fs");
class El{constructor(t){this.nodeType=1;this.tagName=t.toUpperCase();this.children=[];this.attrs={};this.style={};this._t="";this.open=false;}
  append(...k){for(const c of k.flat()){if(c==null)continue;this.children.push(typeof c==="object"?c:Object.assign(new El("#text"),{_t:String(c)}));}}
  replaceChildren(...k){this.children=[];this.append(...k);} setAttribute(k,v){this.attrs[k]=v;} addEventListener(){}
  set textContent(v){this._t=String(v);this.children=[];} get textContent(){return this._t+this.children.map(c=>c.textContent).join("");}
  set className(v){this.attrs.class=v;} get className(){return this.attrs.class;} get scrollHeight(){return 0}}
const ids={};
global.document={createElement:t=>new El(t),createTextNode:x=>Object.assign(new El("#text"),{_t:x}),createElementNS:(n,t)=>new El(t),querySelector:s=>ids[s]||(ids[s]=new El("div")),addEventListener(){}};
global.sessionStorage=global.localStorage={getItem:()=>null,setItem(){},removeItem(){}};
global.scrollTo=()=>{};global.scrollY=0;global.setInterval=()=>{};global.fetch=async()=>({ok:true,json:async()=>({})});
global.location={reload(){}};
const src=fs.readFileSync(process.argv[2],"utf8").replace(/^tick\(\);setInterval\(tick,15000\);/m,"").replace(/^logTabs\(\);logTick\(true\);setInterval\(logTick,3000\);/m,"");
eval(src+"\nrender(JSON.parse(fs.readFileSync(process.argv[3],'utf8')));");
const app=ids["#app"].children[0];
const titles=[];if(!app)throw new Error("nothing rendered");const walk=(e,depth)=>{for(const c of e.children){if(c.tagName==="H2")titles.push("  ".repeat(depth)+c.textContent);if(c.tagName==="SUMMARY")titles.push("[fold] "+c.textContent);if(c.tagName==="DETAILS"&&c.attrs.class==="group")walk(c,1);}};
walk(app,0);console.log(titles.join("\n"));
