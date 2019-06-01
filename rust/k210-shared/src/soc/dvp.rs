use core::cmp;
use k210_hal::pac;
use pac::dvp;

use crate::soc::sleep::usleep;
use crate::soc::sysctl;

pub trait DVPExt: Sized {
    /// Constrains DVP peripheral
    fn constrain(self) -> DVP;
}

impl DVPExt for pac::DVP {
    fn constrain(self) -> DVP { DVP { dvp: self, sccb_addr_len: sccb_addr_len::W8 } }
}

/** Output mode (RGB565 for display, or for planar for AI) */
#[derive(Copy, Clone)]
pub enum output_mode {
    AI,
    DISPLAY,
}

/** SCCB register address width */
#[derive(Copy, Clone)]
pub enum sccb_addr_len {
    W8,
    W16,
}

/** Interrupt */
#[derive(Copy, Clone)]
pub enum interrupt {
    frame_start,
    frame_finish,
}

pub struct DVP {
    dvp: pac::DVP,
    sccb_addr_len: sccb_addr_len,
}

pub type image_format = dvp::dvp_cfg::FORMATW;

impl DVP {
    /** Set SCCB clock to a safe and deterministic value (as low as possible) */
    fn sccb_clk_init(&self) {
        unsafe {
            self.dvp.sccb_cfg.modify(|_,w|
                w.scl_lcnt().bits(255)
                 .scl_hcnt().bits(255)
            );
        }
    }

    /** Set SCCB clock rate */
    pub fn sccb_set_clk_rate(&self, clk_rate: u32) -> u32 {
        let v_sccb_freq = sysctl::clock_get_freq(sysctl::clock::APB1);
        let v_period_clk_cnt = v_sccb_freq / clk_rate / 2; // TODO: round() i.s.o. truncate?

        if v_period_clk_cnt > 255 {
            return 0;
        }
        unsafe {
            self.dvp.sccb_cfg.modify(|_,w|
                w.scl_lcnt().bits(v_period_clk_cnt as u8)
                 .scl_hcnt().bits(v_period_clk_cnt as u8)
            );
        }
        // confused: why does this use clock::DVP but the period_clk_cnt uses clock::APB1?
        return sysctl::clock_get_freq(sysctl::clock::DVP) / (v_period_clk_cnt * 2);
    }

    /** Perform, and wait for a SCCB transfer (read or write) */
    fn sccb_start_transfer(&self) {
        while self.dvp.sts.read().sccb_en().bit() {
            // IDLE
        }
        self.dvp.sts.write(|w| w.sccb_en().set_bit()
                                .sccb_en_we().set_bit());
        while self.dvp.sts.read().sccb_en().bit() {
            // IDLE
        }
    }

    /** Set a register value through SCCB */
    pub fn sccb_send_data(&self, dev_addr: u8, reg_addr: u16, reg_data: u8) {
        use dvp::sccb_cfg::BYTE_NUMW::*;
        unsafe {
            match self.sccb_addr_len {
                sccb_addr_len::W8 => {
                    self.dvp.sccb_cfg.modify(|_,w| w.byte_num().variant(NUM3));
                    self.dvp.sccb_ctl.write(|w| w.device_address().bits(dev_addr | 1)
                                             .reg_address().bits(reg_addr as u8)
                                             .wdata_byte0().bits(reg_data));
                },
                sccb_addr_len::W16 => {
                    self.dvp.sccb_cfg.modify(|_,w| w.byte_num().variant(NUM4));
                    self.dvp.sccb_ctl.write(|w| w.device_address().bits(dev_addr | 1)
                                             .reg_address().bits((reg_addr >> 8) as u8)
                                             .wdata_byte0().bits((reg_addr & 0xff) as u8)
                                             .wdata_byte1().bits(reg_data));
                },
            }
        }
        self.sccb_start_transfer();
    }

    /** Receive register value through SCCB */
    pub fn sccb_receive_data(&self, dev_addr: u8, reg_addr: u16) -> u8 {
        // Write read request
        use dvp::sccb_cfg::BYTE_NUMW::*;
        unsafe {
            match self.sccb_addr_len {
                sccb_addr_len::W8 => {
                    self.dvp.sccb_cfg.modify(|_,w| w.byte_num().variant(NUM2));
                    self.dvp.sccb_ctl.write(|w| w.device_address().bits(dev_addr | 1)
                                             .reg_address().bits(reg_addr as u8));
                },
                sccb_addr_len::W16 => {
                    self.dvp.sccb_cfg.modify(|_,w| w.byte_num().variant(NUM3));
                    self.dvp.sccb_ctl.write(|w| w.device_address().bits(dev_addr | 1)
                                             .reg_address().bits((reg_addr >> 8) as u8)
                                             .wdata_byte0().bits((reg_addr & 0xff) as u8));
                },
            }
        }
        self.sccb_start_transfer();
        // Start read transfer
        unsafe { self.dvp.sccb_ctl.write(|w| w.device_address().bits(dev_addr)); }
        self.sccb_start_transfer();
        self.dvp.sccb_cfg.read().rdata().bits()
    }

    /** Reset DVP-connected device */
    fn reset(&self) {
        /* First power down */
        self.dvp.cmos_cfg.modify(|_,w| w.power_down().set_bit());
        usleep(200_000);
        self.dvp.cmos_cfg.modify(|_,w| w.power_down().clear_bit());
        usleep(200_000);

        /* Second reset */
        self.dvp.cmos_cfg.modify(|_,w| w.reset().clear_bit());
        usleep(200_000);
        self.dvp.cmos_cfg.modify(|_,w| w.reset().set_bit());
        usleep(200_000);
    }

    /** Initialize DVP peripheral */
    pub fn init(&mut self, sccb_addr_len: sccb_addr_len) {
        self.sccb_addr_len = sccb_addr_len;
        sysctl::clock_enable(sysctl::clock::DVP);
        sysctl::reset(sysctl::reset::DVP);
        // Set XCLK to hardcoded divider (3+1)*2=8
        unsafe {
            self.dvp.cmos_cfg.modify(|_,w| w.clk_div().bits(3)
                                            .clk_enable().set_bit());
        }
        self.sccb_clk_init();
        self.reset();
    }

    /** Set XCLK clock rate */
    pub fn set_xclk_rate(&self, xclk_rate: u32) -> u32 {
        // Taken directly from SDK: it's strange that this clock is relative to APB1 not DVP clock
        let v_apb1_clk = sysctl::clock_get_freq(sysctl::clock::APB1);
        let v_period = if v_apb1_clk > xclk_rate * 2 {
            cmp::min((v_apb1_clk / (xclk_rate * 2)) - 1, 255) // TODO round instead of trunc?
        } else {
            0
        };
        unsafe {
            self.dvp.cmos_cfg.modify(|_,w| w.clk_div().bits(v_period as u8)
                                            .clk_enable().set_bit());
        }
        self.reset();
        v_apb1_clk / ((v_period + 1) * 2)
    }

    /** Set input image format */
    pub fn set_image_format(&self, format: image_format) {
        self.dvp.dvp_cfg.modify(|_,w| w.format().variant(format));
    }

    // Not sure it's consistent interface to have two separate functions here, but just going with
    // the SDK for now... might even want to merge these into set_image_size, or force configuring
    // the entire peripheral (register size, burst mode, image size) at once
    /** Enable burst mode */
    pub fn enable_burst(&self) {
        self.dvp.dvp_cfg.modify(|_,w| w.burst_size_4beats().set_bit());
        self.dvp.axi.modify(|_,w| w.gm_mlen().variant(dvp::axi::GM_MLENW::BYTE4));
    }

    /** Disable burst mode */
    pub fn disable_burst(&self) {
        self.dvp.dvp_cfg.modify(|_,w| w.burst_size_4beats().clear_bit());
        self.dvp.axi.modify(|_,w| w.gm_mlen().variant(dvp::axi::GM_MLENW::BYTE1));
    }

    /** Set image size. If burst mode is enabled the maximum configurable size is
     * 8160x1023, without burst mode it is 2040x1023.
     */
    pub fn set_image_size(&self, width: u16, height: u16) {
        // Note: this uses state written in enable/disable_burst, so that needs to be configured before this
        let burst_num = if self.dvp.dvp_cfg.read().burst_size_4beats().bit() {
            width / 8 / 4
        } else {
            width / 8 / 1
        };
        assert!(burst_num < 256);
        assert!(height < 1024);
        unsafe {
            self.dvp.dvp_cfg.modify(|_,w| w.href_burst_num().bits(burst_num as u8)
                                           .line_num().bits(height));
        }
    }

    /** Set address for planar RGB output */
    pub fn set_ai_addr(&self, r_addr: *mut u8, g_addr: *mut u8, b_addr: *mut u8) {
        // Makes use of the fact that
        // a) physical memory is the same as virtual memory on the K210
        // b) memory wraps around every 2^32
        unsafe {
            self.dvp.r_addr.write(|w| w.bits(((r_addr as usize) & 0xffffffff) as u32));
            self.dvp.g_addr.write(|w| w.bits(((g_addr as usize) & 0xffffffff) as u32));
            self.dvp.b_addr.write(|w| w.bits(((b_addr as usize) & 0xffffffff) as u32));
        }
    }

    /** Set address for 16-bit R5G6B5 output */
    pub fn set_display_addr(&self, addr: *mut u16) {
        unsafe {
            self.dvp.rgb_addr.write(|w| w.bits(((addr as usize) & 0xffffffff) as u32));
        }
    }

    /** Start a frame */
    pub fn start_frame(&self) {
        while !self.dvp.sts.read().frame_start().bit() {
            // IDLE
        }
        self.dvp.sts.write(|w| w.frame_start().set_bit()
                                .frame_start_we().set_bit());
    }

    /** Start conversion of frame */
    pub fn start_convert(&self) {
        self.dvp.sts.write(|w| w.dvp_en().set_bit()
                                .dvp_en_we().set_bit());
    }

    /** Finish conversion of frame */
    pub fn finish_convert(&self) {
        while !self.dvp.sts.read().frame_finish().bit() {
            // IDLE
        }
        self.dvp.sts.write(|w| w.frame_finish().set_bit()
                                .frame_finish_we().set_bit());
    }

    /** Wait for an entire frame to complete */
    pub fn get_image(&self) {
        while !self.dvp.sts.read().frame_start().bit() {
            // IDLE
        }
        self.dvp.sts.write(|w| w.frame_start().set_bit()
                                .frame_start_we().set_bit());
        while !self.dvp.sts.read().frame_start().bit() {
            // IDLE
        }
        self.dvp.sts.write(|w| w.frame_finish().set_bit()
                                .frame_finish_we().set_bit()
                                .frame_start().set_bit()
                                .frame_start_we().set_bit()
                                .dvp_en().set_bit()
                                .dvp_en_we().set_bit());
        while !self.dvp.sts.read().frame_finish().bit() {
            // IDLE
        }
    }

    /** Configure interrupt */
    pub fn config_interrupt(&self, interrupt: interrupt, enable: bool) {
        match interrupt {
            interrupt::frame_start => {
                self.dvp.dvp_cfg.modify(|_,w| w.start_int_enable().bit(enable));
            }
            interrupt::frame_finish => {
                self.dvp.dvp_cfg.modify(|_,w| w.finish_int_enable().bit(enable));
            }
        }
    }

    /** Get status of an interrupt */
    pub fn get_interrupt(&self, interrupt: interrupt) -> bool {
        let sts = self.dvp.sts.read();
        match interrupt {
            interrupt::frame_start => { sts.frame_start().bit() }
            interrupt::frame_finish => { sts.frame_finish().bit() }
        }
    }

    /** Clear an interrupt */
    pub fn clear_interrupt(&self, interrupt: interrupt) {
        match interrupt {
            interrupt::frame_start => {
                self.dvp.sts.modify(|_,w| w.frame_start().set_bit()
                                           .frame_start_we().set_bit());
            }
            interrupt::frame_finish => {
                self.dvp.sts.modify(|_,w| w.frame_finish().set_bit()
                                           .frame_finish_we().set_bit());
            }
        }
    }

    /** Enable automatic frame mode */
    pub fn enable_auto(&self) {
        self.dvp.dvp_cfg.modify(|_,w| w.auto_enable().set_bit());
    }

    /** Disable automatic frame mode */
    pub fn disable_auto(&self) {
        self.dvp.dvp_cfg.modify(|_,w| w.auto_enable().clear_bit());
    }

    // The following function could be merged into setting the address?
    // set_display_addr / set_ai_addr with an Option maybe?
    /** Enable/disable an output */
    pub fn set_output_enable(&self, index: output_mode, enable: bool) {
        match index {
            output_mode::AI => {
                self.dvp.dvp_cfg.modify(|_,w| w.ai_output_enable().bit(enable));
            }
            output_mode::DISPLAY => {
                self.dvp.dvp_cfg.modify(|_,w| w.display_output_enable().bit(enable));
            }
        }
    }

}
