#!/usr/bin/env node
// header comment
import { x } from "./x";
/* block
   comment */
@Component({})
class A {
  @Input() name: string;
  @HostListener("click")
  onClick(e) {
    if (e && e.a || e?.b ?? 0) { return 1; }
    const f = (v) => v ? 1 : 2;
    switch (e) { case 1: break; case 2: break; default: ; }
    ;
  }
}
module.exports.handler = function () {
  for (const k in x) { while (k) { do {} while (k); } }
  try { } catch (err) { }
};
const obj = { "key-a": () => 1, method() { return 2; } };
