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
;;! offset_guard_size = 0
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
    i32.store offset=0xffff0000)

  (func (export "do_load") (param i64) (result i32)
    local.get 0
    i32.load offset=0xffff0000))

;; function u0:0:
;; block0:
;;   movn w8, #65531
;;   adds x10, x0, x8
;;   b.lo 8 ; udf
;;   ldr x11, [x2, #8]
;;   subs xzr, x10, x11
;;   b.ls label1 ; b label3
;; block1:
;;   ldr x12, [x2]
;;   movz x13, #65535, LSL #16
;;   add x13, x13, x0
;;   str w1, [x13, x12]
;;   b label2
;; block2:
;;   ret
;; block3:
;;   udf #0xc11f
;;
;; function u0:1:
;; block0:
;;   movn w8, #65531
;;   adds x10, x0, x8
;;   b.lo 8 ; udf
;;   ldr x11, [x1, #8]
;;   subs xzr, x10, x11
;;   b.ls label1 ; b label3
;; block1:
;;   ldr x12, [x1]
;;   movz x11, #65535, LSL #16
;;   add x11, x11, x0
;;   ldr w0, [x11, x12]
;;   b label2
;; block2:
;;   ret
;; block3:
;;   udf #0xc11f