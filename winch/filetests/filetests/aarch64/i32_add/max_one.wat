;;! target = "aarch64"

(module
    (func (result i32)
	(i32.const 0x80000000)
	(i32.const -1)
	(i32.add)
    )
)
;;    0:	 fd7bbfa9             	stp	x29, x30, [sp, #-0x10]!
;;    4:	 fd030091             	mov	x29, sp
;;    8:	 fc030091             	mov	x28, sp
;;    c:	 f08361b2             	orr	x16, xzr, #0xffffffff80000000
;;   10:	 e003102a             	mov	w0, w16
;;   14:	 10008092             	mov	x16, #-1
;;   18:	 0060300b             	add	w0, w0, w16, uxtx
;;   1c:	 fd7bc1a8             	ldp	x29, x30, [sp], #0x10
;;   20:	 c0035fd6             	ret	
