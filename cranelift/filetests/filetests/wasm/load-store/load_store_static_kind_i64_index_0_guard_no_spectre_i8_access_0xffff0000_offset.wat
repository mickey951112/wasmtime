;;! target = "x86_64"
;;!
;;! settings = ['enable_heap_access_spectre_mitigation=false']
;;!
;;! compile = false
;;!
;;! [globals.vmctx]
;;! type = "i64"
;;! vmctx = true
;;!
;;! [globals.heap_base]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 0, readonly = true }
;;!
;;! # (no heap_bound global for static heaps)
;;!
;;! [[heaps]]
;;! base = "heap_base"
;;! min_size = 0x10000
;;! offset_guard_size = 0
;;! index_type = "i64"
;;! style = { kind = "static", bound = 0x10000000 }

;; !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
;; !!! GENERATED BY 'make-load-store-tests.sh' DO NOT EDIT !!!
;; !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!

(module
  (memory i64 1)

  (func (export "do_store") (param i64 i32)
    local.get 0
    local.get 1
    i32.store8 offset=0xffff0000)

  (func (export "do_load") (param i64) (result i32)
    local.get 0
    i32.load8_u offset=0xffff0000))

;; function u0:0(i64, i32, i64 vmctx) fast {
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly gv0
;;     heap0 = static gv1, min 0x0001_0000, bound 0x1000_0000, offset_guard 0, index_type i64
;;
;;                                 block0(v0: i64, v1: i32, v2: i64):
;; @0040                               v3 = heap_addr.i64 heap0, v0, 0xffff_0000, 1
;; @0040                               istore8 little heap v1, v3
;; @0047                               jump block1
;;
;;                                 block1:
;; @0047                               return
;; }
;;
;; function u0:1(i64, i64 vmctx) -> i32 fast {
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly gv0
;;     heap0 = static gv1, min 0x0001_0000, bound 0x1000_0000, offset_guard 0, index_type i64
;;
;;                                 block0(v0: i64, v1: i64):
;; @004c                               v3 = heap_addr.i64 heap0, v0, 0xffff_0000, 1
;; @004c                               v4 = uload8.i32 little heap v3
;; @0053                               jump block1(v4)
;;
;;                                 block1(v2: i32):
;; @0053                               return v2
;; }