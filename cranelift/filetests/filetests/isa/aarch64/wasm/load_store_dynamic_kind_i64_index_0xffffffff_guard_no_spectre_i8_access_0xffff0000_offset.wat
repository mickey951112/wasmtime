;;! target = "aarch64"
;;!
;;! settings = ['enable_heap_access_spectre_mitigation=false']
;;!
;;! compile = true
;;!
;;! [globals.vmctx]
;;! type = "i64"
;;! vmctx = true
;;!
;;! [globals.heap_base]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 0, readonly = true }
;;!
;;! [globals.heap_bound]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 8, readonly = true }
;;!
;;! [[heaps]]
;;! base = "heap_base"
;;! min_size = 0x10000
;;! offset_guard_size = 0xffffffff
;;! index_type = "i64"
;;! style = { kind = "dynamic", bound = "heap_bound" }

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

;; function u0:0:
;; block0:
;;   ldr x7, [x2, #8]
;;   subs xzr, x0, x7
;;   b.hi label3 ; b label1
;; block1:
;;   ldr x9, [x2]
;;   movz x10, #65535, LSL #16
;;   add x10, x10, x0
;;   strb w1, [x10, x9]
;;   b label2
;; block2:
;;   ret
;; block3:
;;   udf #0xc11f
;;
;; function u0:1:
;; block0:
;;   ldr x7, [x1, #8]
;;   subs xzr, x0, x7
;;   b.hi label3 ; b label1
;; block1:
;;   ldr x9, [x1]
;;   movz x8, #65535, LSL #16
;;   add x8, x8, x0
;;   ldrb w0, [x8, x9]
;;   b label2
;; block2:
;;   ret
;; block3:
;;   udf #0xc11f
