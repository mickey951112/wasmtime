;;! target = "x86_64"

(module
    (func (param i64) (result i64)
        (local.get 0)
        (i64.clz)
    )
)
;;    0:	 55                   	push	rbp
;;    1:	 4889e5               	mov	rbp, rsp
;;    4:	 4883ec10             	sub	rsp, 0x10
;;    8:	 48897c2408           	mov	qword ptr [rsp + 8], rdi
;;    d:	 4c893424             	mov	qword ptr [rsp], r14
;;   11:	 488b442408           	mov	rax, qword ptr [rsp + 8]
;;   16:	 480fbdc0             	bsr	rax, rax
;;   1a:	 41bb00000000         	mov	r11d, 0
;;   20:	 410f95c3             	setne	r11b
;;   24:	 48f7d8               	neg	rax
;;   27:	 4883c040             	add	rax, 0x40
;;   2b:	 4c29d8               	sub	rax, r11
;;   2e:	 4883c410             	add	rsp, 0x10
;;   32:	 5d                   	pop	rbp
;;   33:	 c3                   	ret	
