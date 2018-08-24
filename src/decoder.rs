//! AO1 Decoder
//!
//!

use ffi::aom::*;
use std::marker::PhantomData;
use std::mem::{uninitialized, zeroed};
use std::mem;
use std::ptr;
use std::sync::Arc;
use common::AOMCodec;
use data::frame::{Frame, VideoInfo};
use data::frame::{PictureType, new_default_frame};
use data::pixel::formats::YUV420;

fn frame_from_img(img: aom_image_t) -> Frame {
    let f = match img.fmt {
        aom_img_fmt_AOM_IMG_FMT_I420 => YUV420,
        _ => panic!("TODO: support more pixel formats"),
    };
    let v = VideoInfo {
        pic_type: PictureType::UNKNOWN,
        width: img.d_w as usize,
        height: img.d_h as usize,
        format: Arc::new(*f),
    };

    let mut f = new_default_frame(v, None);

    let src = img.planes.iter().map(|v| *v as *const u8);
    let linesize = img.stride.iter().map(|l| *l as usize);

    f.copy_from_raw_parts(src, linesize);
    f
}

/// AO1 Decoder
pub struct AV1Decoder<T> {
    pub(crate) ctx: aom_codec_ctx,
    pub(crate) iter: aom_codec_iter_t,
    private_data: PhantomData<T>
}

impl<T> AV1Decoder<T> {
    /// Create a new decoder
    pub fn new() -> Result<AV1Decoder<T>, aom_codec_err_t> {
        let mut dec = AV1Decoder {
            ctx: unsafe { uninitialized() },
            iter: ptr::null(),
            private_data: PhantomData,
        };
        let cfg = unsafe { zeroed() };

        let ret = unsafe {
            aom_codec_dec_init_ver(
                &mut dec.ctx as *mut aom_codec_ctx,
                aom_codec_av1_dx(),
                &cfg as *const aom_codec_dec_cfg_t,
                0,
                AOM_DECODER_ABI_VERSION as i32,
            )
        };
        match ret {
            aom_codec_err_t_AOM_CODEC_OK => Ok(dec),
            _ => Err(ret),
        }
    }

    /// Feed some compressed data to the encoder
    ///
    /// The `data` slice is sent to the decoder alongside the optional
    /// `private` struct.
    ///
    /// The [`get_frame`] method must be called to retrieve the decompressed
    /// frame, do not call this method again before calling [`get_frame`].
    ///
    /// It matches a call to `aom_codec_decode`.
    ///
    /// [`get_frame`]: #method.get_frame
    pub fn decode<O>(&mut self, data: &[u8], private: O) -> Result<(), aom_codec_err_t>
        where O: Into<Option<T>> {
        let priv_data = private
            .into()
            .map(|v| {
                Box::into_raw(Box::new(v))
            })
            .unwrap_or(ptr::null_mut());
        let ret = unsafe {
            aom_codec_decode(
                &mut self.ctx,
                data.as_ptr(),
                data.len(),
                mem::transmute(priv_data)
            )
        };

        // Safety measure to not call get_frame on an invalid iterator
        self.iter = ptr::null();

        match ret {
            aom_codec_err_t_AOM_CODEC_OK => Ok(()),
            _ => {
                let _ = unsafe { Box::from_raw(priv_data) };
                Err(ret)
            },
        }
    }


    /// Notify the decoder to return any pending frame
    ///
    /// The [`get_frame`] method must be called to retrieve the decompressed
    /// frame.
    ///
    /// It matches a call to `aom_codec_decode` with NULL arguments.
    ///
    /// [`get_frame`]: #method.get_frame
    pub fn flush(&mut self) -> Result<(), aom_codec_err_t> {
        let ret = unsafe {
             aom_codec_decode(
                &mut self.ctx,
                ptr::null(),
                0,
                ptr::null_mut(),
            )
        };

        self.iter = ptr::null();

        match ret {
            aom_codec_err_t_AOM_CODEC_OK=> {
                Ok(())
            },
            _ => Err(ret),
        }
    }

    /// Retrieve decoded frames
    ///
    /// Should be called repeatedly until it returns `None`.
    ///
    /// It matches a call to `aom_codec_get_frame`.
    pub fn get_frame(&mut self) -> Option<(Frame, Option<Box<T>>)> {
        let img = unsafe { aom_codec_get_frame(&mut self.ctx, &mut self.iter) };
        mem::forget(img);

        if img.is_null() {
            None
        } else {
            let im = unsafe { *img };
            let priv_data = if im.user_priv.is_null() {
                None
            } else {
                let p : *mut T = unsafe { mem::transmute(im.user_priv) };
                Some(unsafe { Box::from_raw(p) })
            };
            let frame = frame_from_img(im);
            Some((frame, priv_data))
        }
    }
}

impl<T> Drop for AV1Decoder<T> {
    fn drop(&mut self) {
        unsafe { aom_codec_destroy(&mut self.ctx) };
    }
}

impl<T> AOMCodec for AV1Decoder<T> {
    fn get_context(&mut self) -> &mut aom_codec_ctx {
        &mut self.ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn init() {
        let mut d = AV1Decoder::<()>::new().unwrap();

        println!("{}", d.error_to_str());
    }

    use super::super::encoder::tests as enc;
    use super::super::encoder::AOMPacket;
    use data::timeinfo::TimeInfo;
    use data::rational::*;
    #[test]
    fn decode() {
        let w = 800;
        let h = 600;

        let t = TimeInfo {
            pts: Some(0),
            dts: Some(0),
            duration: Some(1),
            timebase: Some(Rational64::new(1, 1000)),
            user_private: None,
        };

        let mut e = enc::setup(w, h, &t);
        let mut f = enc::setup_frame(w, h, &t);

        let mut d = AV1Decoder::<()>::new().unwrap();
        let mut out = 0;

        for i in 0..100 {
            e.encode(&f).unwrap();
            f.t.pts = Some(i);

            println!("{:#?}", f);
            loop {
                let p = e.get_packet();

                if p.is_none() {
                    break;
                } else {
                    if let AOMPacket::Packet(ref pkt) = p.unwrap() {
                        let _ = d.decode(&pkt.data, None).unwrap();

                        // No multiframe expected.
                        if let Some(f) = d.get_frame() {
                            out = 1;
                            println!("{:#?}", f);
                        }
                    }
                }
            }
        }

        if out != 1 {
            panic!("No frame decoded");
        }
    }

}
