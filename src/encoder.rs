//! Encoding functionality
//!
//!

use common::AOMCodec;
use ffi::aom::*;

use std::mem;
use std::ptr;

use data::frame::{Frame, MediaKind, FrameBufferConv};
use data::pixel::Formaton;
use data::pixel::formats::YUV420;
use data::packet::Packet;

#[derive(Clone, Debug, PartialEq)]
pub struct PSNR {
    pub samples: [u32; 4],
    pub sse: [u64; 4],
    pub psnr: [f64; 4],
}

/// Safe wrapper around `aom_codec_cx_pkt`
#[derive(Clone, Debug)]
pub enum AOMPacket {
    Packet(Packet),
    Stats(Vec<u8>),
    MBStats(Vec<u8>),
    PSNR(PSNR),
    Custom(Vec<u8>),
}

fn to_buffer(buf: aom_fixed_buf_t) -> Vec<u8> {
    let mut v: Vec<u8> = Vec::with_capacity(buf.sz);
    unsafe {
        ptr::copy_nonoverlapping(mem::transmute(buf.buf), v.as_mut_ptr(), buf.sz);
        v.set_len(buf.sz);
    }
    v
}

impl AOMPacket {
    fn new(pkt: aom_codec_cx_pkt) -> AOMPacket {
        match pkt.kind {
            aom_codec_cx_pkt_kind_AOM_CODEC_CX_FRAME_PKT => {
                let f = unsafe { pkt.data.frame };
                let mut p = Packet::with_capacity(f.sz);
                unsafe {
                    ptr::copy_nonoverlapping(mem::transmute(f.buf), p.data.as_mut_ptr(), f.sz);
                    p.data.set_len(f.sz);
                }
                p.t.pts = Some(f.pts);
                p.is_key = (f.flags & AOM_FRAME_IS_KEY) != 0;

                AOMPacket::Packet(p)
            }
            aom_codec_cx_pkt_kind_AOM_CODEC_STATS_PKT => {
                let b = to_buffer(unsafe { pkt.data.twopass_stats });
                AOMPacket::Stats(b)
            }
            aom_codec_cx_pkt_kind_AOM_CODEC_FPMB_STATS_PKT => {
                let b = to_buffer(unsafe { pkt.data.firstpass_mb_stats });
                AOMPacket::MBStats(b)
            }
            aom_codec_cx_pkt_kind_AOM_CODEC_PSNR_PKT => {
                let p = unsafe { pkt.data.psnr };

                AOMPacket::PSNR(PSNR {
                    samples: p.samples,
                    sse: p.sse,
                    psnr: p.psnr,
                })
            }
            aom_codec_cx_pkt_kind_AOM_CODEC_CUSTOM_PKT => {
                let b = to_buffer(unsafe { pkt.data.raw });
                AOMPacket::Custom(b)
            }
            _ => panic!("No packet defined")
        }
    }
}

pub struct AV1EncoderConfig {
    pub cfg: aom_codec_enc_cfg,
}

unsafe impl Send for AV1EncoderConfig {} // TODO: Make sure it cannot be abused

// TODO: Extend
fn map_formaton(img: &mut aom_image, fmt: &Formaton) {
    if fmt == YUV420 {
        img.fmt = aom_img_fmt_AOM_IMG_FMT_I420;
    } else {
        unimplemented!();
    }
    img.bit_depth = 8;
    img.bps = 12;
    img.x_chroma_shift = 1;
    img.y_chroma_shift = 1;
}

fn img_from_frame<'a>(frame: &'a Frame) -> aom_image {
    let mut img: aom_image = unsafe { mem::zeroed() };

    if let MediaKind::Video(ref v) = frame.kind {
        map_formaton(&mut img, &v.format);
        img.d_w = v.width as u32;
        img.d_h = v.height as u32;
    }
    // populate the buffers
    for i in 0..frame.buf.count() {
        let s: &[u8] = frame.buf.as_slice(i).unwrap();
        img.planes[i] = unsafe { mem::transmute(s.as_ptr()) };
        img.stride[i] = frame.buf.linesize(i).unwrap() as i32;
    }

    img
}

// TODO: provide a builder?

/// AV1 Encoder setup facility
impl AV1EncoderConfig {
    /// Create a new default configuration
    pub fn new() -> Result<AV1EncoderConfig, aom_codec_err_t> {
        let mut cfg = unsafe { mem::uninitialized() };
        let ret = unsafe { aom_codec_enc_config_default(aom_codec_av1_cx(), &mut cfg, 0) };

        match ret {
            aom_codec_err_t_AOM_CODEC_OK => Ok(AV1EncoderConfig { cfg: cfg }),
            _ => Err(ret),
        }
    }

    /// Return a newly allocated `AV1Encoder` using the current configuration
    pub fn get_encoder(&mut self) -> Result<AV1Encoder, aom_codec_err_t> {
        AV1Encoder::new(self)
    }
}

/// AV1 Encoder
pub struct AV1Encoder {
    pub(crate) ctx: aom_codec_ctx_t,
    pub(crate) iter: aom_codec_iter_t,
}

unsafe impl Send for AV1Encoder {} // TODO: Make sure it cannot be abused

impl AV1Encoder {
    /// Create a new encoder using the provided configuration
    ///
    /// You may use `get_encoder` instead.
    pub fn new(cfg: &mut AV1EncoderConfig) -> Result<AV1Encoder, aom_codec_err_t> {
        let mut ctx = unsafe { mem::uninitialized() };
        let ret = unsafe {
            aom_codec_enc_init_ver(
                &mut ctx,
                aom_codec_av1_cx(),
                &mut cfg.cfg,
                0,
                AOM_ENCODER_ABI_VERSION as i32,
            )
        };

        match ret {
            aom_codec_err_t_AOM_CODEC_OK => Ok(AV1Encoder {
                ctx: ctx,
                iter: ptr::null(),
            }),
            _ => Err(ret),
        }
    }

    /// Update the encoder parameters after-creation
    ///
    /// It calls `aom_codec_control_`
    pub fn control(&mut self, id: aome_enc_control_id, val: i32) -> Result<(), aom_codec_err_t> {
        let ret = unsafe { aom_codec_control_(&mut self.ctx, id as i32, val) };

        match ret {
            aom_codec_err_t_AOM_CODEC_OK => Ok(()),
            _ => Err(ret),
        }
    }

    // TODO: Cache the image information
    //
    /// Send an uncompressed frame to the encoder
    ///
    /// Call [`get_packet`] to receive the compressed data.
    ///
    /// It calls `aom_codec_encode`.
    ///
    /// [`get_packet`]: #method.get_packet
    pub fn encode(&mut self, frame: &Frame) -> Result<(), aom_codec_err_t> {
        let mut img = img_from_frame(frame);

        let ret = unsafe {
            aom_codec_encode(
                &mut self.ctx,
                &mut img,
                frame.t.pts.unwrap(),
                1,
                0
            )
        };

        self.iter = ptr::null();

        match ret {
            aom_codec_err_t_AOM_CODEC_OK => Ok(()),
            _ => Err(ret),
        }
    }

    /// Notify the encoder that no more data will be sent
    ///
    /// Call [`get_packet`] to receive the compressed data.
    ///
    /// It calls `vpx_codec_encode` with NULL arguments.
    ///
    /// [`get_packet`]: #method.get_packet
    pub fn flush(&mut self) -> Result<(), aom_codec_err_t> {
        let ret = unsafe {
             aom_codec_encode(
                &mut self.ctx,
                ptr::null_mut(),
                0,
                1,
                0
            )
        };

        self.iter = ptr::null();

        match ret {
            aom_codec_err_t_AOM_CODEC_OK => Ok(()),
            _ => Err(ret),
        }
    }

    /// Retrieve the compressed data
    ///
    /// To be called until it returns `None`.
    ///
    /// It calls `vpx_codec_get_cx_data`.
    pub fn get_packet(&mut self) -> Option<AOMPacket> {
        let pkt = unsafe { aom_codec_get_cx_data(&mut self.ctx, &mut self.iter) };

        if pkt.is_null() {
            None
        } else {
            Some(AOMPacket::new(unsafe { *pkt }))
        }
    }
}

impl Drop for AV1Encoder {
    fn drop(&mut self) {
        unsafe { aom_codec_destroy(&mut self.ctx) };
    }
}


impl AOMCodec for AV1Encoder {
    fn get_context<'a>(&'a mut self) -> &'a mut aom_codec_ctx {
        &mut self.ctx
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn init() {
        let mut c = AV1EncoderConfig::new().unwrap();
        let mut e = c.get_encoder().unwrap();
        println!("{}", e.error_to_str());
    }

    use data::timeinfo::TimeInfo;
    use data::rational::*;
    pub fn setup(w: u32, h: u32, t: &TimeInfo) -> AV1Encoder {
        let mut c = AV1EncoderConfig::new().unwrap();
        c.cfg.g_w = w;
        c.cfg.g_h = h;
        c.cfg.g_timebase.num = *t.timebase.unwrap().numer() as i32;
        c.cfg.g_timebase.den = *t.timebase.unwrap().denom() as i32;
        c.cfg.g_threads = 4;
        c.cfg.g_pass = aom_enc_pass_AOM_RC_ONE_PASS;
        c.cfg.rc_end_usage =  aom_rc_mode_AOM_CQ;

        let mut e = c.get_encoder().unwrap();

        e.control(aome_enc_control_id_AOME_SET_CQ_LEVEL, 4).unwrap();

        e
    }

    pub fn setup_frame(w: u32, h: u32, t: &TimeInfo) -> Frame {
        use data::pixel::formats;
        use data::frame::*;
        use std::sync::Arc;

        let v = VideoInfo {
            pic_type: PictureType::UNKNOWN,
            width: w as usize,
            height: h as usize,
            format: Arc::new(*formats::YUV420),
        };

        new_default_frame(v, Some(t.clone()))
    }

    #[test]
    fn encode() {
        let w = 200;
        let h = 200;

        let t = TimeInfo {
            pts: Some(0),
            dts: Some(0),
            duration: Some(1),
            timebase: Some(Rational64::new(1, 1000)),
            user_private: None,
        };

        let mut e = setup(w, h, &t);
        let mut f = setup_frame(w, h, &t);

        let mut out = 0;
        // TODO write some pattern
        for i in 0..100 {
            e.encode(&f).unwrap();
            f.t.pts = Some(i);
            println!("{:#?}", f);
            loop {
                let p = e.get_packet();

                if p.is_none() {
                    break;
                } else {
                    out = 1;
                    println!("{:#?}", p.unwrap());
                }
            }
        }

        if out != 1 {
            panic!("No packet produced");
        }
    }
}
