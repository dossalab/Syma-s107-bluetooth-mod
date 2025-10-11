pub(super) const THREAD_PENDER: usize = usize::MAX;

use core::marker::PhantomData;
use cortex_m::asm;
use embassy_nrf::pac;

use embassy_executor::{raw, Spawner};

pub struct MwuWorkaroundExecutor {
    inner: raw::Executor,
    not_send: PhantomData<*mut ()>,
}

impl MwuWorkaroundExecutor {
    pub fn new() -> Self {
        Self {
            inner: raw::Executor::new(THREAD_PENDER as *mut ()),
            not_send: PhantomData,
        }
    }

    #[inline(always)]
    fn mwu_enable() {
        pac::MWU.regionenset().write(|w| {
            w.set_rgn_wa(0, true);
            w.set_prgn_wa(0, true);
        });
    }

    #[inline(always)]
    fn mwu_disable() {
        pac::MWU.regionenclr().write(|w| {
            w.set_rgn_wa(0, true);
            w.set_prgn_wa(0, true);
        });
    }

    pub fn run(&'static mut self, init: impl FnOnce(Spawner)) -> ! {
        init(self.inner.spawner());
        loop {
            unsafe {
                self.inner.poll();
                // nRF52832 errata: MWU: Increased current consumption
                // high current consumption with MWU enabled
                // The only workaround is to disable MWU while going into sleep

                Self::mwu_disable();
                asm::wfe();

                asm::nop();
                asm::nop();
                asm::nop();
                asm::nop();

                Self::mwu_enable();
            };
        }
    }
}
