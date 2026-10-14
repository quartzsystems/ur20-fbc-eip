// Copyright (C), 2026 Quartz Systems. Some rights reserved. This work is
// licensed under the terms of the MIT license which can be found in the
// root directory of this project.

//! The [`CipService`] trait: anything that can carry a Message Router request.
//!
//! [`Client`](crate::Client) implements it over TCP. Higher layers are generic over it so they
//! can be tested against an in-memory mock.

use std::future::Future;

use crate::epath::{AttributePath, ObjectPath};
use crate::error::Result;
use crate::identity::{IDENTITY_OBJECT, Identity};
use crate::message::{MessageRequest, MessageResponse, service};

/// Explicit messaging transport.
pub trait CipService: Send {
    /// Send one request and return the raw response (status not yet checked).
    fn send(
        &mut self,
        request: MessageRequest,
    ) -> impl Future<Output = Result<MessageResponse>> + Send;

    /// Send a request and return its data, failing on non-zero CIP status.
    fn call(&mut self, request: MessageRequest) -> impl Future<Output = Result<Vec<u8>>> + Send {
        async move {
            let response = self.send(request.clone()).await?;
            response.into_result(&request)
        }
    }

    /// Get_Attribute_Single (0x0E).
    fn get_attribute_single(
        &mut self,
        path: AttributePath,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send {
        self.call(MessageRequest::new(
            service::GET_ATTRIBUTE_SINGLE,
            path,
            Vec::new(),
        ))
    }

    /// Set_Attribute_Single (0x10).
    fn set_attribute_single(
        &mut self,
        path: AttributePath,
        value: &[u8],
    ) -> impl Future<Output = Result<()>> + Send {
        let request = MessageRequest::new(service::SET_ATTRIBUTE_SINGLE, path, value.to_vec());
        async move { self.call(request).await.map(|_| ()) }
    }

    /// Get_Attributes_All (0x01).
    fn get_attributes_all(
        &mut self,
        path: ObjectPath,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send {
        self.call(MessageRequest::new(
            service::GET_ATTRIBUTES_ALL,
            path,
            Vec::new(),
        ))
    }

    /// Read the Identity Object (class 0x01, instance 1).
    fn identity(&mut self) -> impl Future<Output = Result<Identity>> + Send {
        async move {
            let data = self.get_attributes_all(IDENTITY_OBJECT).await?;
            Identity::decode(&data)
        }
    }
}
