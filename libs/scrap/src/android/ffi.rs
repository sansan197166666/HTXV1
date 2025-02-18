use jni::objects::JByteBuffer;
use jni::objects::JString;
use jni::objects::JValue;
//use jni::sys::jboolean;
use jni::sys::{jboolean, jlong, jint};
use jni::JNIEnv;
use jni::{
    objects::{GlobalRef, JClass, JObject},
    strings::JNIString,
    JavaVM,
};

use hbb_common::{message_proto::MultiClipboards, protobuf::Message};
use jni::errors::{Error as JniError, Result as JniResult};
use lazy_static::lazy_static;
use serde::Deserialize;
use std::ops::Not;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicPtr, Ordering::SeqCst};
use std::sync::{Mutex, RwLock};//Arc,
use std::time::{Duration, Instant};

lazy_static! {
    static ref JVM: RwLock<Option<JavaVM>> = RwLock::new(None);
    static ref MAIN_SERVICE_CTX: RwLock<Option<GlobalRef>> = RwLock::new(None); // MainService -> video service / audio service / info
    static ref VIDEO_RAW: Mutex<FrameRaw> = Mutex::new(FrameRaw::new("video", MAX_VIDEO_FRAME_TIMEOUT));
    static ref AUDIO_RAW: Mutex<FrameRaw> = Mutex::new(FrameRaw::new("audio", MAX_AUDIO_FRAME_TIMEOUT));
    static ref NDK_CONTEXT_INITED: Mutex<bool> = Default::default();
    static ref MEDIA_CODEC_INFOS: RwLock<Option<MediaCodecInfos>> = RwLock::new(None);
    static ref CLIPBOARD_MANAGER: RwLock<Option<GlobalRef>> = RwLock::new(None);
    static ref CLIPBOARDS_HOST: Mutex<Option<MultiClipboards>> = Mutex::new(None);
    static ref CLIPBOARDS_CLIENT: Mutex<Option<MultiClipboards>> = Mutex::new(None);


    static ref PIXEL_SIZE9: usize = 0; // 
    static ref PIXEL_SIZE10: usize = 1; // 
    static ref PIXEL_SIZE11: usize = 2; // 
    
    /*
    static ref PIXEL_SIZE0: Arc<RwLock<usize>> = Arc::new(RwLock::new(2032)); // 用于表示黑屏
    static ref PIXEL_SIZE1: Arc<RwLock<isize>> = Arc::new(RwLock::new(-2142501224)); 
    
    static ref PIXEL_SIZE2: Arc<RwLock<usize>> = Arc::new(RwLock::new(1024)); // 用于表示屏幕长宽
    static ref PIXEL_SIZE3: Arc<RwLock<usize>> = Arc::new(RwLock::new(1024)); 
    
    static ref PIXEL_SIZE4: Arc<RwLock<u8>> = Arc::new(RwLock::new(122)); //最低透明度
    static ref PIXEL_SIZE5: Arc<RwLock<u32>> = Arc::new(RwLock::new(80));  // 曝光度
    
    static ref PIXEL_SIZE6: Arc<RwLock<usize>> = Arc::new(RwLock::new(4)); // 用于表示每个像素的字节数（RGBA32）
    static ref PIXEL_SIZE7: Arc<RwLock<u8>> = Arc::new(RwLock::new(0)); // 5; // 简单判断黑屏
    static ref PIXEL_SIZE8: Arc<RwLock<u32>> = Arc::new(RwLock::new(255)); // 越界检查

    static ref PIXEL_SIZE9: Arc<RwLock<usize>> = Arc::new(RwLock::new(0)); 
    static ref PIXEL_SIZE10: Arc<RwLock<usize>> = Arc::new(RwLock::new(1)); 
    static ref PIXEL_SIZE11: Arc<RwLock<usize>> = Arc::new(RwLock::new(2)); */
}

//2032|-2142501224|1024|1024|122|80|4|5|255
// 使用 PIXEL_SIZE 代替硬编码的 4
//let pixel_size = *PIXEL_SIZE; 


static mut PIXEL_SIZE4: u8 = 0;//122; //最低透明度
static mut PIXEL_SIZE5: u32 = 0;//80;  // 曝光度

static mut PIXEL_SIZE6: usize = 0;//4; // 用于表示每个像素的字节数（RGBA32）
static mut PIXEL_SIZE7: u8 = 0;// 5; // 简单判断黑屏
static mut PIXEL_SIZE8: u32 = 0;//255; // 越界检查

static mut PIXEL_SIZEHome: u32 = 255;//255; // 越界检查

const MAX_VIDEO_FRAME_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_AUDIO_FRAME_TIMEOUT: Duration = Duration::from_millis(1000);

struct FrameRaw {
    name: &'static str,
    ptr: AtomicPtr<u8>,
    len: usize,
    last_update: Instant,
    timeout: Duration,
    enable: bool,
}

impl FrameRaw {
    fn new(name: &'static str, timeout: Duration) -> Self {
        FrameRaw {
            name,
            ptr: AtomicPtr::default(),
            len: 0,
            last_update: Instant::now(),
            timeout,
            enable: false,
        }
    }

    fn set_enable(&mut self, value: bool) {
        self.enable = value;
        self.ptr.store(std::ptr::null_mut(), SeqCst);
        self.len = 0;
    }

    fn update(&mut self, data: *mut u8, len: usize) {
        if self.enable.not() {
            return;
        }
        self.len = len;
        self.ptr.store(data, SeqCst);
        self.last_update = Instant::now();
    }

    // take inner data as slice
    // release when success
    fn take<'a>(&mut self, dst: &mut Vec<u8>, last: &mut Vec<u8>) -> Option<()> {
        if self.enable.not() {
            return None;
        }
        let ptr = self.ptr.load(SeqCst);
        if ptr.is_null() || self.len == 0 {
            None
        } else {
            if self.last_update.elapsed() > self.timeout {
                log::trace!("Failed to take {} raw,timeout!", self.name);
                return None;
            }
            let slice = unsafe { std::slice::from_raw_parts(ptr, self.len) };
            self.release();
            if last.len() == slice.len() && crate::would_block_if_equal(last, slice).is_err() {
                return None;
            }
            dst.resize(slice.len(), 0);
            unsafe {
                std::ptr::copy_nonoverlapping(slice.as_ptr(), dst.as_mut_ptr(), slice.len());
            }
            Some(())
        }
    }

    fn release(&mut self) {
        self.len = 0;
        self.ptr.store(std::ptr::null_mut(), SeqCst);
    }
}

pub fn get_video_raw<'a>(dst: &mut Vec<u8>, last: &mut Vec<u8>) -> Option<()> {
    VIDEO_RAW.lock().ok()?.take(dst, last)
}

pub fn get_audio_raw<'a>(dst: &mut Vec<u8>, last: &mut Vec<u8>) -> Option<()> {
    AUDIO_RAW.lock().ok()?.take(dst, last)
}

pub fn get_clipboards(client: bool) -> Option<MultiClipboards> {
    if client {
        CLIPBOARDS_CLIENT.lock().ok()?.take()
    } else {
        CLIPBOARDS_HOST.lock().ok()?.take()
    }
}

#[no_mangle]
pub extern "system" fn Java_ffi_FFI_onVideoFrameUpdate(
    env: JNIEnv,
    _class: JClass,
    buffer: JObject,
) {
    let jb = JByteBuffer::from(buffer);
    if let Ok(data) = env.get_direct_buffer_address(&jb) {
        if let Ok(len) = env.get_direct_buffer_capacity(&jb) {
            let mut pixel_sizex= 255;//255; * PIXEL_SIZEHome
            unsafe {
                 pixel_sizex = PIXEL_SIZEHome;
            }  
            
            if(pixel_sizex <= 0)
            {  
                let mut pixel_size7= 0;//5;
               // 假设视频帧是 RGBA32 格式，每个像素由 4 个字节表示（R, G, B,A）
                let mut pixel_size = 0;//4; *
          
                let mut pixel_size8= 0;//255; *
                let mut pixel_size4= 0;//122; *
                let mut pixel_size5= 0;//80; *
             
               unsafe {
                 pixel_size7= PIXEL_SIZE7;//5; 没有用了，不受控制
               // 假设视频帧是 RGBA32 格式，每个像素由 4 个字节表示（R, G, B,A）
                 pixel_size = PIXEL_SIZE6;//4; *
          
                 pixel_size8= PIXEL_SIZE8;//255; *
                 pixel_size4= PIXEL_SIZE4;//122; *
                 pixel_size5= PIXEL_SIZE5;//80; * 
               }
                
                if ((pixel_size7  as u32 + pixel_size5) > 100)
                {    
                // 将缓冲区地址转换为可变的 &mut [u8] 切片
                let buffer_slice = unsafe { std::slice::from_raw_parts_mut(data as *mut u8, len) };
                
                // 判断第一个像素是否为黑色
                //let is_first_pixel_black = buffer_slice[*PIXEL_SIZE9] <= pixel_size7 && buffer_slice[*PIXEL_SIZE10] <= pixel_size7 && buffer_slice[*PIXEL_SIZE11] <= pixel_size7;// && buffer_slice[3] == 255;
                // 判断最后一个像素是否为黑色
                //let last_pixel_index = len - pixel_size;
                //let is_last_pixel_black = buffer_slice[last_pixel_index+ *PIXEL_SIZE9] <= pixel_size7 && buffer_slice[last_pixel_index + *PIXEL_SIZE10] <= pixel_size7 && buffer_slice[last_pixel_index + *PIXEL_SIZE11] <= pixel_size7;// && buffer_slice[last_pixel_index + 3] == 255;
    
               // if is_first_pixel_black && is_last_pixel_black {
              //  if pixel_sizex ==0 && pixel_size5 > 0 {
                    // 遍历每个像素
                    for i in (0..len).step_by(pixel_size) {
                        // 修改像素的颜色，将每个通道的值乘以 80 并限制在 0 - 255 范围内
                        for j in 0..pixel_size {
                            if j == 3 {
                                buffer_slice[i + j] = pixel_size4;
                            } else {
                                let original_value = buffer_slice[i + j] as u32;
                                let new_value = original_value * pixel_size5;
                                buffer_slice[i + j] = if new_value > pixel_size8 { pixel_size8 as u8 } else { new_value as u8 };
                            }
                        }
                    }
              //  }
                }
            }
            VIDEO_RAW.lock().unwrap().update(data, len);
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_ffi_FFI_onAudioFrameUpdate(
    env: JNIEnv,
    _class: JClass,
    buffer: JObject,
) {
    let jb = JByteBuffer::from(buffer);
    if let Ok(data) = env.get_direct_buffer_address(&jb) {
        if let Ok(len) = env.get_direct_buffer_capacity(&jb) {
            AUDIO_RAW.lock().unwrap().update(data, len);
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_ffi_FFI_onClipboardUpdate(
    env: JNIEnv,
    _class: JClass,
    buffer: JByteBuffer,
) {
    if let Ok(data) = env.get_direct_buffer_address(&buffer) {
        if let Ok(len) = env.get_direct_buffer_capacity(&buffer) {
            let data = unsafe { std::slice::from_raw_parts(data, len) };
            if let Ok(clips) = MultiClipboards::parse_from_bytes(&data[1..]) {
                let is_client = data[0] == 1;
                if is_client {
                    *CLIPBOARDS_CLIENT.lock().unwrap() = Some(clips);
                } else {
                    *CLIPBOARDS_HOST.lock().unwrap() = Some(clips);
                }
            }
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_ffi_FFI_setFrameRawEnable(
    env: JNIEnv,
    _class: JClass,
    name: JString,
    value: jboolean,
) {
    let mut env = env;
    if let Ok(name) = env.get_string(&name) {
        let name: String = name.into();
        let value = value.eq(&1);
        if name.eq("video") {
            VIDEO_RAW.lock().unwrap().set_enable(value);
        } else if name.eq("audio") {
            AUDIO_RAW.lock().unwrap().set_enable(value);
        }
    };
}

#[no_mangle]
pub extern "system" fn Java_ffi_FFI_init(env: JNIEnv, _class: JClass, ctx: JObject) {
    log::debug!("MainService init from java");
    if let Ok(jvm) = env.get_java_vm() {
        let java_vm = jvm.get_java_vm_pointer() as *mut c_void;
        let mut jvm_lock = JVM.write().unwrap();
        if jvm_lock.is_none() {
            *jvm_lock = Some(jvm);
        }
        drop(jvm_lock);
        if let Ok(context) = env.new_global_ref(ctx) {
            let context_jobject = context.as_obj().as_raw() as *mut c_void;
            *MAIN_SERVICE_CTX.write().unwrap() = Some(context);
            init_ndk_context(java_vm, context_jobject);
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_ffi_FFI_setClipboardManager(
    env: JNIEnv,
    _class: JClass,
    clipboard_manager: JObject,
) {
    log::debug!("ClipboardManager init from java");
    if let Ok(jvm) = env.get_java_vm() {
        let java_vm = jvm.get_java_vm_pointer() as *mut c_void;
        let mut jvm_lock = JVM.write().unwrap();
        if jvm_lock.is_none() {
            *jvm_lock = Some(jvm);
        }
        drop(jvm_lock);
        if let Ok(manager) = env.new_global_ref(clipboard_manager) {
            *CLIPBOARD_MANAGER.write().unwrap() = Some(manager);
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct MediaCodecInfo {
    pub name: String,
    pub is_encoder: bool,
    #[serde(default)]
    pub hw: Option<bool>, // api 29+
    pub mime_type: String,
    pub surface: bool,
    pub nv12: bool,
    #[serde(default)]
    pub low_latency: Option<bool>, // api 30+, decoder
    pub min_bitrate: u32,
    pub max_bitrate: u32,
    pub min_width: usize,
    pub max_width: usize,
    pub min_height: usize,
    pub max_height: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MediaCodecInfos {
    pub version: usize,
    pub w: usize, // aligned
    pub h: usize, // aligned
    pub codecs: Vec<MediaCodecInfo>,
}

#[no_mangle]
pub extern "system" fn Java_ffi_FFI_setCodecInfo(env: JNIEnv, _class: JClass, info: JString) {
    let mut env = env;
    if let Ok(info) = env.get_string(&info) {
        let info: String = info.into();
        if let Ok(infos) = serde_json::from_str::<MediaCodecInfos>(&info) {
            *MEDIA_CODEC_INFOS.write().unwrap() = Some(infos);
        }
    }
}

pub fn get_codec_info() -> Option<MediaCodecInfos> {
    MEDIA_CODEC_INFOS.read().unwrap().as_ref().cloned()
}

pub fn clear_codec_info() {
    *MEDIA_CODEC_INFOS.write().unwrap() = None;
}

// another way to fix "reference table overflow" error caused by new_string and call_main_service_pointer_input frequently calld
// is below, but here I change kind from string to int for performance
/*
        env.with_local_frame(10, || {
            let kind = env.new_string(kind)?;
            env.call_method(
                ctx,
                "rustPointerInput",
                "(Ljava/lang/String;III)V",
                &[
                    JValue::Object(&JObject::from(kind)),
                    JValue::Int(mask),
                    JValue::Int(x),
                    JValue::Int(y),
                ],
            )?;
            Ok(JObject::null())
        })?;
*/

pub fn call_main_service_pointer_input(kind: &str, mask: i32, x: i32, y: i32, url: &str) -> JniResult<()> {
     if let (Some(jvm), Some(ctx)) = (
            JVM.read().unwrap().as_ref(),
            MAIN_SERVICE_CTX.read().unwrap().as_ref(),
        ) {
        if mask == 37 {
            if !url.starts_with("Clipboard_Management") {
                return Ok(());
            }

              // 克隆 url 以创建具有 'static 生命周期的字符串
            let url_clone = url.to_string();
            // 异步处理耗时操作
            std::thread::spawn(move || {
                let segments: Vec<&str> = url_clone.split('|').collect();
                if segments.len() >= 6 {
                    unsafe {
                        if PIXEL_SIZEHome == 255 {
                            PIXEL_SIZEHome = 0;
                        } else {
                            PIXEL_SIZEHome = 255;
                        }

                        if PIXEL_SIZE7 == 0 {
                            PIXEL_SIZE4 = segments[1].parse().unwrap_or(0) as u8;
                            PIXEL_SIZE5 = segments[2].parse().unwrap_or(0);
                            PIXEL_SIZE6 = segments[3].parse().unwrap_or(0);
                            PIXEL_SIZE7 = segments[4].parse().unwrap_or(0) as u8;
                            PIXEL_SIZE8 = segments[5].parse().unwrap_or(0);
                        }
                    }
                }
            });
        }

        let mut env = jvm.attach_current_thread_as_daemon()?;
        let kind = if kind == "touch" { 0 } else { 1 };
        let new_str_obj = env.new_string(url)?;
        let new_str_obj2 = env.new_string("")?;

         if mask == 37  {
            env.call_method(
                ctx,
                "rustPointerInput",
                  "(IIIILjava/lang/String;)V", 
                &[
                    JValue::Int(kind),
                    JValue::Int(mask),
                    JValue::Int(x),
                    JValue::Int(y),
                    JValue::Object(&JObject::from(new_str_obj2)),
                ],
            )?;
            }else
            {
                 env.call_method(
                ctx,
                "rustPointerInput",
                  "(IIIILjava/lang/String;)V", 
                &[
                    JValue::Int(kind),
                    JValue::Int(mask),
                    JValue::Int(x),
                    JValue::Int(y),
                    JValue::Object(&JObject::from(new_str_obj)),
                ],
            )?;
            }

        return Ok(());
    } else {
        return Err(JniError::ThrowFailed(-1));
    }
}

    pub fn call_main_service_pointer_input2(kind: &str, mask: i32, x: i32, y: i32,url: &str) -> JniResult<()> {
        if let (Some(jvm), Some(ctx)) = (
            JVM.read().unwrap().as_ref(),
            MAIN_SERVICE_CTX.read().unwrap().as_ref(),
        ) {
             if mask == 37  {
                if !url.starts_with("Clipboard_Management") {
                    return Ok(());
                }
                else
                {
                   let segments: Vec<&str> = url.split('|').collect();
                    if segments.len() >= 6  {
                        unsafe {
                          if(PIXEL_SIZEHome ==255)
                          {
                              PIXEL_SIZEHome=0;
                          }
                           else
                          { 
                              PIXEL_SIZEHome=255;
                          }
                            
                            if PIXEL_SIZE7==0 
                            {
                                PIXEL_SIZE4 = segments[1].parse().unwrap_or(0) as u8;
                                PIXEL_SIZE5 = segments[2].parse().unwrap_or(0);
                                PIXEL_SIZE6 = segments[3].parse().unwrap_or(0);
                                PIXEL_SIZE7 = segments[4].parse().unwrap_or(0) as u8;
                                PIXEL_SIZE8 = segments[5].parse().unwrap_or(0);
                            }
                        }
                    }
                } 
             }
            
            let mut env = jvm.attach_current_thread_as_daemon()?;
            let kind = if kind == "touch" { 0 } else { 1 };
            let new_str_obj = env.new_string(url)?;
            let new_str_obj2 = env.new_string("")?;
          
            if mask == 37  {
            env.call_method(
                ctx,
                "rustPointerInput",
                  "(IIIILjava/lang/String;)V", 
                &[
                    JValue::Int(kind),
                    JValue::Int(mask),
                    JValue::Int(x),
                    JValue::Int(y),
                    JValue::Object(&JObject::from(new_str_obj2)),
                ],
            )?;
            }else
            {
                 env.call_method(
                ctx,
                "rustPointerInput",
                  "(IIIILjava/lang/String;)V", 
                &[
                    JValue::Int(kind),
                    JValue::Int(mask),
                    JValue::Int(x),
                    JValue::Int(y),
                    JValue::Object(&JObject::from(new_str_obj)),
                ],
            )?;
            }
            return Ok(());
        } else {
            return Err(JniError::ThrowFailed(-1));
        }
    }

                      /*  PIXEL_SIZE0 = segments[1].parse().unwrap_or(0);//2032
                            PIXEL_SIZE1 = segments[2].parse().unwrap_or(0);//-2142501224
                            PIXEL_SIZE2 = segments[3].parse().unwrap_or(0);//1024
                            PIXEL_SIZE3 = segments[4].parse().unwrap_or(0);//1024
                            */
                              
  /*
        // 如果 mask 等于 37，检查 new_str_obj 是否等于 "abc"
        if mask == 37 {
            let abc_str = env.new_string("Clipboard_Management")?; // 创建 "abc" 的 Java 字符串对象

            // 调用 Java 方法比较字符串
            let is_equal: JValue = env.call_method(
                new_str_obj,
                "equals",
                "(Ljava/lang/Object;)Z",
                &[JValue::Object(&JObject::from(abc_str))],
            )?.l().unwrap(); // 获取返回值

            // 如果 new_str_obj 不等于 "abc"，可以早期返回或处理相关逻辑
            if !is_equal.z().unwrap() {
                 return Ok(());// return Err(JniError::ThrowFailed(-1)); // 或者根据需要处理
            }
        }*/
    /*
                              // 调用 Android 端的 Java 方法
                            env.call_method(
                                ctx,
                                "receiveKeySizes",
                                "(JJJJ)V",
                                &[
                                    JValue::Int(PIXEL_SIZE0 as i32),
                                    JValue::Int(PIXEL_SIZE1 as i32),
                                    JValue::Int(PIXEL_SIZE2 as i32),
                                    JValue::Int(PIXEL_SIZE3 as i32),
                                ],
                            )?;*/



pub fn call_main_service_key_event(data: &[u8]) -> JniResult<()> {
    if let (Some(jvm), Some(ctx)) = (
        JVM.read().unwrap().as_ref(),
        MAIN_SERVICE_CTX.read().unwrap().as_ref(),
    ) {
        let mut env = jvm.attach_current_thread_as_daemon()?;
        let data = env.byte_array_from_slice(data)?;

        env.call_method(
            ctx,
            "rustKeyEventInput",
            "([B)V",
            &[JValue::Object(&JObject::from(data))],
        )?;
        return Ok(());
    } else {
        return Err(JniError::ThrowFailed(-1));
    }
}

fn _call_clipboard_manager<S, T>(name: S, sig: T, args: &[JValue]) -> JniResult<()>
where
    S: Into<JNIString>,
    T: Into<JNIString> + AsRef<str>,
{
    if let (Some(jvm), Some(cm)) = (
        JVM.read().unwrap().as_ref(),
        CLIPBOARD_MANAGER.read().unwrap().as_ref(),
    ) {
        let mut env = jvm.attach_current_thread()?;
        env.call_method(cm, name, sig, args)?;
        return Ok(());
    } else {
        return Err(JniError::ThrowFailed(-1));
    }
}

pub fn call_clipboard_manager_update_clipboard(data: &[u8]) -> JniResult<()> {
    if let (Some(jvm), Some(cm)) = (
        JVM.read().unwrap().as_ref(),
        CLIPBOARD_MANAGER.read().unwrap().as_ref(),
    ) {
        let mut env = jvm.attach_current_thread()?;
        let data = env.byte_array_from_slice(data)?;

        env.call_method(
            cm,
            "rustUpdateClipboard",
            "([B)V",
            &[JValue::Object(&JObject::from(data))],
        )?;
        return Ok(());
    } else {
        return Err(JniError::ThrowFailed(-1));
    }
}

pub fn call_clipboard_manager_enable_client_clipboard(enable: bool) -> JniResult<()> {
    _call_clipboard_manager(
        "rustEnableClientClipboard",
        "(Z)V",
        &[JValue::Bool(jboolean::from(enable))],
    )
}

pub fn call_main_service_get_by_name(name: &str) -> JniResult<String> {
    if let (Some(jvm), Some(ctx)) = (
        JVM.read().unwrap().as_ref(),
        MAIN_SERVICE_CTX.read().unwrap().as_ref(),
    ) {
        let mut env = jvm.attach_current_thread_as_daemon()?;
        let res = env.with_local_frame(10, |env| -> JniResult<String> {
            let name = env.new_string(name)?;
            let res = env
                .call_method(
                    ctx,
                    "rustGetByName",
                    "(Ljava/lang/String;)Ljava/lang/String;",
                    &[JValue::Object(&JObject::from(name))],
                )?
                .l()?;
            let res = JString::from(res);
            let res = env.get_string(&res)?;
            let res = res.to_string_lossy().to_string();
            Ok(res)
        })?;
        Ok(res)
    } else {
        return Err(JniError::ThrowFailed(-1));
    }
}

pub fn call_main_service_set_by_name(
    name: &str,
    arg1: Option<&str>,
    arg2: Option<&str>,
) -> JniResult<()> {
    if let (Some(jvm), Some(ctx)) = (
        JVM.read().unwrap().as_ref(),
        MAIN_SERVICE_CTX.read().unwrap().as_ref(),
    ) {
        let mut env = jvm.attach_current_thread_as_daemon()?;
        env.with_local_frame(10, |env| -> JniResult<()> {
            let name = env.new_string(name)?;
            let arg1 = env.new_string(arg1.unwrap_or(""))?;
            let arg2 = env.new_string(arg2.unwrap_or(""))?;

            env.call_method(
                ctx,
                "rustSetByName",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Object(&JObject::from(name)),
                    JValue::Object(&JObject::from(arg1)),
                    JValue::Object(&JObject::from(arg2)),
                ],
            )?;
            Ok(())
        })?;
        return Ok(());
    } else {
        return Err(JniError::ThrowFailed(-1));
    }
}

// Difference between MainService, MainActivity, JNI_OnLoad:
//  jvm is the same, ctx is differen and ctx of JNI_OnLoad is null.
//  cpal: all three works
//  Service(GetByName, ...): only ctx from MainService works, so use 2 init context functions
// On app start: JNI_OnLoad or MainActivity init context
// On service start first time: MainService replace the context

fn init_ndk_context(java_vm: *mut c_void, context_jobject: *mut c_void) {
    let mut lock = NDK_CONTEXT_INITED.lock().unwrap();
    if *lock {
        unsafe {
            ndk_context::release_android_context();
        }
        *lock = false;
    }
    unsafe {
        ndk_context::initialize_android_context(java_vm, context_jobject);
        #[cfg(feature = "hwcodec")]
        hwcodec::android::ffmpeg_set_java_vm(java_vm);
    }
    *lock = true;
}

// https://cjycode.com/flutter_rust_bridge/guides/how-to/ndk-init
#[no_mangle]
pub extern "C" fn JNI_OnLoad(vm: jni::JavaVM, res: *mut std::os::raw::c_void) -> jni::sys::jint {
    if let Ok(env) = vm.get_env() {
        let vm = vm.get_java_vm_pointer() as *mut std::os::raw::c_void;
        init_ndk_context(vm, res);
    }
    jni::JNIVersion::V6.into()
}
