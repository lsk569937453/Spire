use crate::vojo::lets_encrypt::LetsEntrypt;
use crate::vojo::{app_error::AppError, base_response::BaseResponse};
use crate::SharedConfig;
use axum::extract::State;
use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder,
    OrderStatus,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};

use std::{io, time::Duration};
use tokio::time::sleep;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct LetsEncryptResponse {
    key_perm: String,
    certificate_perm: String,
}
pub async fn lets_encrypt_certificate(
    State(_): State<SharedConfig>,

    axum::extract::Json(lets_encrypt_object): axum::extract::Json<LetsEntrypt>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    let request_result = lets_encrypt_object.start_request2().await;
    if let Err(err) = request_result {
        return Ok((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        ));
    }
    let certificate = request_result.unwrap();
    let response = LetsEncryptResponse {
        key_perm: certificate.clone(),
        certificate_perm: certificate,
    };
    let data = BaseResponse {
        response_code: 0,
        response_object: certificate_response,
    };

    Ok(Json(data))
}
pub async fn lets_encrypt_certificate(
    State(_): State<SharedConfig>,

    axum::extract::Json(lets_encrypt_object): axum::extract::Json<LetsEntrypt>,
) -> Result<impl axum::response::IntoResponse, AppError> {
    lets_encrypt_certificate_logic(lets_encrypt_object).await
}
#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::to_bytes;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn test_lets_encrypt_certificate_success() {
        let mut mock_le_actions = MockLetsEncryptActions::new();
        mock_le_actions
            .expect_start_request2()
            .times(1)
            .returning(|| Ok("mock_certificate_content".to_string()));
        let response = lets_encrypt_certificate_logic(mock_le_actions)
            .await
            .unwrap();

        let res = response.into_response();

        let (parts, body_data) = res.into_parts();
        let body = to_bytes(body_data, usize::MAX).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::OK);
        let response = serde_json::from_slice::<BaseResponse<LetsEncryptResponse>>(&body);
        assert!(response.is_err());
    }
    #[tokio::test]
    async fn test_lets_encrypt_certificat_error() {
        let mut mock_le_actions = MockLetsEncryptActions::new();
        mock_le_actions
            .expect_start_request2()
            .times(1)
            .returning(|| Err(AppError("mock_certificate_content".to_string())));
        let response = lets_encrypt_certificate_logic(mock_le_actions).await;

        let res = response.into_response();

        let (parts, body_data) = res.into_parts();
        let body = to_bytes(body_data, usize::MAX).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let response = serde_json::from_slice::<String>(&body);
        assert!(response.is_err());
    }
}
pub async fn lets_encrypt_certificate_renew(
    lets_encrypt_object: LetsEntrypt,
) -> Result<(), AppError> {
    let (account, credentials) = Account::create(
        &NewAccount {
            contact: &[],
            terms_of_service_agreed: true,
            only_return_existing: false,
        },
        LetsEncrypt::Staging.url(),
        None,
    )
    .await
    .map_err(|e| AppError(e.to_string()))?;
    info!(
        "account credentials:\n\n{}",
        serde_json::to_string_pretty(&credentials).unwrap()
    );

    let identifier = Identifier::Dns(lets_encrypt_object.domain_name);
    let mut order = account
        .new_order(&NewOrder {
            identifiers: &[identifier],
        })
        .await
        .unwrap();

    let state = order.state();
    info!("order state: {:#?}", state);
    assert!(matches!(state.status, OrderStatus::Pending));

    let authorizations = order.authorizations().await.unwrap();
    let mut challenges = Vec::with_capacity(authorizations.len());
    for authz in &authorizations {
        match authz.status {
            AuthorizationStatus::Pending => {}
            AuthorizationStatus::Valid => continue,
            _ => todo!(),
        }

        // We'll use the DNS challenges for this example, but you could
        // pick something else to use here.

        let challenge = authz
            .challenges
            .iter()
            .find(|c| c.r#type == ChallengeType::Dns01)
            .ok_or_else(|| AppError("no dns01 challenge found".to_string()))?;

        let Identifier::Dns(identifier) = &authz.identifier;

        info!("Please set the following DNS record then press the Return key:");
        info!(
            "_acme-challenge.{} IN TXT {}",
            identifier,
            order.key_authorization(challenge).dns_value()
        );
        io::stdin()
            .read_line(&mut String::new())
            .map_err(|e| AppError(e.to_string()))?;

        challenges.push((identifier, &challenge.url));
    }

    // Let the server know we're ready to accept the challenges.

    for (_, url) in &challenges {
        order.set_challenge_ready(url).await.unwrap();
    }

    // Exponentially back off until the order becomes ready or invalid.

    let mut tries = 1u8;
    let mut delay = Duration::from_millis(250);
    loop {
        sleep(delay).await;
        let state = order.refresh().await.unwrap();
        if let OrderStatus::Ready | OrderStatus::Invalid = state.status {
            info!("order state: {:#?}", state);
            break;
        }

        delay *= 2;
        tries += 1;
        match tries < 5 {
            true => info!("order is not ready, waiting {delay:?},{:?}{}", state, tries),
            false => {
                error!("order is not ready: {state:#?},{}", tries);
                return Err(AppError("order is not ready".to_string()));
            }
        }
    }

    let state = order.state();
    if state.status != OrderStatus::Ready {
        return Err(AppError(format!(
            "unexpected order status: {:?}",
            state.status
        )));
    }

    let mut names = Vec::with_capacity(challenges.len());
    for (identifier, _) in challenges {
        names.push(identifier.to_owned());
    }

    // If the order is ready, we can provision the certificate.
    // Use the rcgen library to create a Certificate Signing Request.

    let mut params = CertificateParams::new(names.clone()).map_err(|e| AppError(e.to_string()))?;
    params.distinguished_name = DistinguishedName::new();
    let private_key = KeyPair::generate().map_err(|e| AppError(e.to_string()))?;
    let csr = params
        .serialize_request(&private_key)
        .map_err(|e| AppError(e.to_string()))?;

    // Finalize the order and print certificate chain, private key and account credentials.

    order.finalize(csr.der()).await.unwrap();
    let cert_chain_pem = loop {
        match order.certificate().await.unwrap() {
            Some(cert_chain_pem) => break cert_chain_pem,
            None => sleep(Duration::from_secs(1)).await,
        }
    };

    info!("certficate chain:\n\n{}", cert_chain_pem);
    info!("private key:\n\n{}", private_key.serialize_pem());
    Ok(())
}
